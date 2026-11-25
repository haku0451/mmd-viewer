//! Model and motion session.
//!
//! File parsing, image decoding, and mip generation run on a background
//! thread. GPU uploads stay on the UI thread.
//!
//! Physics runs in step with the motion clock, as in MikuMikuDance. Each
//! motion frame (1/30 s) the animated pose is evaluated and the Bullet world
//! advances four 1/120 s steps toward it, so the solved hair and cloth always
//! belong to the body drawn with them. While playback is stopped the world
//! keeps stepping on the display clock until the pose comes to rest.
//!
//! Each model evaluates and simulates on its own worker thread, which owns
//! its Bullet world. The UI thread sends every model its request, then waits
//! for all of them, so several models run in parallel and the frame that is
//! drawn is always complete. When a display tick falls behind by more than
//! one motion frame, the world still advances by one frame: the motion jumps
//! ahead, and the kinematic bodies cover the jump over that frame's steps.
//! Catching up step by step would make the late frame later still.
//!
//! During playback the next motion frame is requested as soon as the current
//! one is shown, so the workers use the display ticks in between and a new
//! frame rarely waits. A request that playback no longer needs (pause, reset,
//! a new motion, a skipped frame) is collected and dropped, and the frame on
//! screen is evaluated again.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use glam::{Mat4, Quat, Vec3};
use mmd_animation::{EvaluatedFrame, evaluate};
use mmd_asset::{CpuTexture, load_model_textures, load_shared_toons};
use mmd_core::{FrameTime, ModelAsset, Motion};
use mmd_format::{load_model, load_motion};
use mmd_physics::{PhysicsWorld, SimulationFlags};
use mmd_platform_windows::{OpenFileKind, pick_open_file};
use mmd_renderer::{DrawMaterial, ImageUpload, SkinnedModel};

use crate::audio::StageAudio;

/// Viewer state for the loaded models, motions, and playback.
pub struct Session {
    actors: Vec<Actor>,
    selected: usize,
    /// Current frame. Every model shares this timeline.
    pub frame: i32,
    pub playing: bool,
    /// The viewport uses the motion camera when one is available.
    pub follow_camera: bool,
    pub ground_shadow: bool,
    /// Self shadow. Off by default, as in MikuMikuDance.
    pub self_shadow: bool,
    /// Floor squares and the colored axes, as one guide.
    pub show_guides: bool,
    pub info_open: bool,
    /// Frame number and frames per second, drawn on the viewport.
    pub show_stats: bool,
    /// The dancers are shown in a separate window and the main view is black.
    pub stage_open: bool,
    pub stage_fullscreen: bool,
    pub message: String,
    pub loading: bool,
    /// Bullet steps while this is on, including while playback is stopped.
    pub physics_enabled: bool,
    /// Viewport refresh rate. Motion frames stay at 30 fps.
    pub frame_rate: FrameRate,
    audio: StageAudio,
    frame_dt: f32,
    fps: f32,
    accum: f32,
    incoming: Option<mpsc::Receiver<LoadEvent>>,
}

/// One model, its motion, and the worker that owns its physics world.
struct Actor {
    model: Arc<ModelAsset>,
    gpu: SkinnedModel,
    motion: Option<Arc<Motion>>,
    matched_bones: usize,
    /// Evaluated frame with the solved bones written over the animated ones.
    pose: Option<EvaluatedFrame>,
    worker: PoseWorker,
    physics_reset: bool,
    physics_settled: bool,
    /// The worker is already evaluating this frame for playback.
    prefetched: Option<i32>,
    geometry_dirty: bool,
    path: String,
}

/// Work for one model's pose thread.
enum PoseRequest {
    Motion(Option<Arc<Motion>>),
    /// Optionally evaluate a frame, then advance the world `elapsed` seconds.
    Run {
        frame: Option<i32>,
        elapsed: f32,
        flags: SimulationFlags,
    },
}

/// The answer to [`PoseRequest::Run`].
struct PoseReply {
    /// The newly evaluated frame, with solved bones, when a frame was asked for.
    frame: Option<Box<EvaluatedFrame>>,
    translation: Vec<Vec3>,
    rotation: Vec<Quat>,
    skin: Vec<Mat4>,
    settled: bool,
}

/// A thread that evaluates one model and steps its Bullet world.
///
/// The world is built on the thread and never leaves it.
struct PoseWorker {
    requests: Option<mpsc::Sender<PoseRequest>>,
    replies: mpsc::Receiver<PoseReply>,
    pending: bool,
    thread: Option<JoinHandle<()>>,
}

struct PoseThread {
    model: Arc<ModelAsset>,
    motion: Option<Arc<Motion>>,
    world: PhysicsWorld,
    /// Animated bones of the last evaluated frame, the solver's target.
    translation: Vec<Vec3>,
    rotation: Vec<Quat>,
    skin: Vec<Mat4>,
}

enum LoadEvent {
    Model(Result<LoadedModel, String>),
    Motion(Result<Motion, String>),
    Audio(Result<(kira::sound::static_sound::StaticSoundData, String), String>),
}

/// How often the viewport presents a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRate {
    Fps30,
    Fps60,
    Unlimited,
}

impl FrameRate {
    /// Time between presents. Unlimited has no period.
    pub fn period(self) -> Option<Duration> {
        match self {
            Self::Fps30 => Some(Duration::from_secs_f32(1.0 / 30.0)),
            Self::Fps60 => Some(Duration::from_secs_f32(1.0 / 60.0)),
            Self::Unlimited => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fps30 => "30 FPS",
            Self::Fps60 => "60 FPS",
            Self::Unlimited => "無制限",
        }
    }
}

/// One model ready to draw.
pub struct ActorDraw<'a> {
    pub gpu: &'a SkinnedModel,
    pub pose: &'a EvaluatedFrame,
    /// Vertex positions changed and should be uploaded once.
    pub upload_vertices: bool,
}

struct LoadedModel {
    asset: ModelAsset,
    textures: Vec<CpuTexture>,
    toons: Vec<CpuTexture>,
    path: PathBuf,
}

/// Text shown in the model-information window.
pub struct ModelCard {
    pub name: String,
    pub english_name: String,
    pub comment: String,
    pub path: String,
    pub vertices: usize,
    pub faces: usize,
    pub materials: usize,
    pub bones: usize,
    pub morphs: usize,
    pub rigid_bodies: usize,
    pub joints: usize,
}

impl Session {
    pub fn new() -> Self {
        Self {
            actors: Vec::new(),
            selected: 0,
            frame: 0,
            playing: false,
            follow_camera: false,
            ground_shadow: true,
            self_shadow: false,
            show_guides: true,
            info_open: false,
            show_stats: false,
            stage_open: false,
            stage_fullscreen: false,
            message: "モデル未読み込み".to_string(),
            loading: false,
            physics_enabled: true,
            frame_rate: FrameRate::Fps60,
            audio: StageAudio::new(),
            frame_dt: 0.0,
            fps: 0.0,
            accum: 0.0,
            incoming: None,
        }
    }

    /// The model that receives the next motion.
    pub fn model(&self) -> Option<&Arc<ModelAsset>> {
        self.actors.get(self.selected).map(|actor| &actor.model)
    }

    pub fn motion(&self) -> Option<&Motion> {
        self.actors
            .get(self.selected)
            .and_then(|actor| actor.motion.as_deref())
    }

    /// Bone tracks on the selected model that match a bone name.
    pub fn matched_bones(&self) -> usize {
        self.actors
            .get(self.selected)
            .map(|actor| actor.matched_bones)
            .unwrap_or(0)
    }

    /// Names in load order. The selected entry is marked by [`selected`](Self::selected).
    pub fn actor_names(&self) -> Vec<String> {
        self.actors
            .iter()
            .map(|actor| {
                if actor.model.name.is_empty() {
                    "名称なし".to_string()
                } else {
                    actor.model.name.clone()
                }
            })
            .collect()
    }

    /// Index of the model that receives a motion.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Choose which model receives the next motion.
    pub fn select(&mut self, index: usize) {
        if index < self.actors.len() {
            self.selected = index;
        }
    }

    /// Drop the selected model and its physics.
    pub fn remove_selected(&mut self) {
        if self.selected < self.actors.len() {
            self.actors.remove(self.selected);
            if self.selected >= self.actors.len() {
                self.selected = self.actors.len().saturating_sub(1);
            }
        }
        if self.actors.is_empty() {
            self.playing = false;
            self.audio.stop();
            self.info_open = false;
            self.message = "モデル未読み込み".to_string();
        }
    }

    pub fn model_card(&self) -> Option<ModelCard> {
        let actor = self.actors.get(self.selected)?;
        let name = if actor.model.name.is_empty() {
            "名称なし".to_string()
        } else {
            actor.model.name.clone()
        };
        Some(ModelCard {
            name,
            english_name: actor.model.english_name.clone(),
            comment: actor.model.comment.clone(),
            path: actor.path.clone(),
            vertices: actor.model.vertices.len(),
            faces: actor.model.indices.len() / 3,
            materials: actor.model.materials.len(),
            bones: actor.model.bones.len(),
            morphs: actor.model.morphs.len(),
            rigid_bodies: actor.model.rigid_bodies.len(),
            joints: actor.model.joints.len(),
        })
    }

    /// Load a file dropped on the window. The extension picks the kind.
    pub fn load_dropped(&mut self, path: PathBuf) {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = match extension.as_str() {
            "pmx" | "pmd" => Some(OpenFileKind::Model),
            "vmd" => Some(OpenFileKind::Motion),
            "wav" | "mp3" | "m4a" | "ogg" => Some(OpenFileKind::Audio),
            _ => None,
        };
        match kind {
            Some(kind) => self.start_load(kind, path),
            None => {
                self.message =
                    "このファイルはモデル、モーション、音声として読み込めません".to_string();
            }
        }
    }

    /// Loaded models, in draw order.
    pub fn draws(&self) -> Vec<ActorDraw<'_>> {
        self.actors
            .iter()
            .filter_map(|actor| {
                actor.pose.as_ref().map(|pose| ActorDraw {
                    gpu: &actor.gpu,
                    pose,
                    upload_vertices: actor.geometry_dirty,
                })
            })
            .collect()
    }

    /// Pose used for the motion camera. The selected model wins when it has one.
    pub fn camera_pose(&self) -> Option<&EvaluatedFrame> {
        let selected = self
            .actors
            .get(self.selected)
            .and_then(|actor| actor.pose.as_ref());
        if selected.is_some_and(|pose| pose.camera.is_some()) {
            return selected;
        }
        self.actors
            .iter()
            .find_map(|actor| actor.pose.as_ref().filter(|pose| pose.camera.is_some()))
    }

    pub fn audio_label(&self) -> &str {
        self.audio.label()
    }

    /// Morphs on the selected model whose weight is away from zero.
    pub fn active_morphs(&self) -> Vec<(String, f32)> {
        let Some(actor) = self.actors.get(self.selected) else {
            return Vec::new();
        };
        let Some(pose) = &actor.pose else {
            return Vec::new();
        };
        actor
            .model
            .morphs
            .iter()
            .zip(&pose.morph_weights)
            .filter(|(_, weight)| weight.abs() >= 0.001)
            .map(|(morph, weight)| (morph.name().to_string(), *weight))
            .collect()
    }

    /// Turn the physics step on or off and rebuild the displayed pose.
    pub fn set_physics_enabled(&mut self, enabled: bool) {
        self.physics_enabled = enabled;
        self.mark_physics_reset();
        self.run_all(true, 0.0);
    }

    /// Snap rigid bodies back to the current pose and clear their velocity.
    pub fn reset_physics(&mut self) {
        self.mark_physics_reset();
        self.run_all(true, 0.0);
    }

    /// Latest evaluated pose of the selected model.
    pub fn pose(&self) -> Option<&EvaluatedFrame> {
        self.actors
            .get(self.selected)
            .and_then(|actor| actor.pose.as_ref())
    }

    /// Last frame across every loaded motion, so a shorter dancer holds the end.
    pub fn last_frame(&self) -> i32 {
        self.actors
            .iter()
            .filter_map(|actor| actor.motion.as_deref().map(Motion::last_frame))
            .max()
            .unwrap_or(0)
    }

    /// Ask Windows for a file and parse it off the UI thread.
    pub fn open(&mut self, kind: OpenFileKind) {
        if self.loading {
            self.message = "読み込み中です".to_string();
            return;
        }
        let path = match pick_open_file(kind) {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(error) => {
                self.message = format!("ファイルを開けませんでした: {error}");
                return;
            }
        };
        self.start_load(kind, path);
    }

    /// Start or continue playback, including the audio clip at this frame.
    pub fn play(&mut self) {
        let has_camera = self.actors.iter().any(|actor| {
            actor
                .motion
                .as_ref()
                .is_some_and(|motion| !motion.cameras.is_empty())
        });
        if self.actors.is_empty() && !has_camera {
            self.message = "モデル未読み込み".to_string();
            return;
        }
        let resume = self.audio.is_paused();
        self.playing = true;
        if resume {
            self.audio.resume();
        } else {
            self.audio.play_from(f64::from(self.frame) / 30.0);
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.audio.pause();
        // The workers may already have stepped toward the next frame.
        self.run_all(true, 0.0);
    }

    /// Return to frame zero and pause.
    pub fn stop(&mut self) {
        self.playing = false;
        self.accum = 0.0;
        self.frame = 0;
        self.audio.stop();
        self.mark_physics_reset();
        self.run_all(true, 0.0);
    }

    /// Smoothed viewport rate. Zero until a frame has been presented.
    pub fn frames_per_second(&self) -> f32 {
        self.fps
    }

    /// Advance playback by `dt` seconds and evaluate the resulting frame.
    pub fn advance(&mut self, dt: f32) {
        if dt.is_finite() && dt > 0.0 && dt < 0.5 {
            let sample = 1.0 / dt;
            self.fps = if self.fps == 0.0 {
                sample
            } else {
                self.fps * 0.9 + sample * 0.1
            };
        }
        self.frame_dt = dt;
        if self.playing && dt.is_finite() && dt > 0.0 {
            self.accum += dt.min(0.1);
            let step = 1.0 / 30.0;
            let mut frames = 0;
            let mut wrapped = false;
            while self.accum >= step {
                self.accum -= step;
                self.frame += 1;
                if self.frame > self.last_frame() {
                    self.frame = 0;
                    wrapped = true;
                    self.mark_physics_reset();
                }
                frames += 1;
            }
            if wrapped {
                self.audio.play_from(0.0);
            }
            if frames > 0 {
                // Physics time follows the motion clock, at most one frame per
                // tick. A display tick that shows no new frame does not step.
                self.play_frame(step);
            }
        } else if !self.playing && dt.is_finite() && dt > 0.0 {
            self.run_all(false, dt.min(0.1));
        }
    }

    /// Take a finished background load and upload GPU resources.
    pub fn poll(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, sample_count: u32) {
        let Some(receiver) = &self.incoming else {
            return;
        };
        let event = match receiver.try_recv() {
            Ok(event) => event,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading = false;
                self.incoming = None;
                self.message = "読み込みに失敗しました".to_string();
                return;
            }
        };
        self.incoming = None;
        self.loading = false;
        match event {
            LoadEvent::Model(Ok(loaded)) => self.install_model(device, queue, sample_count, loaded),
            LoadEvent::Model(Err(error)) => {
                self.message = format!("モデルを読めませんでした: {error}")
            }
            LoadEvent::Motion(Ok(motion)) => {
                if self.actors.is_empty() {
                    self.message = "先にモデルを読み込んでください".to_string();
                } else {
                    let name = if motion.model_name.is_empty() {
                        "モーション".to_string()
                    } else {
                        motion.model_name.clone()
                    };
                    self.actors[self.selected].set_motion(Some(Arc::new(motion)));
                    self.frame = 0;
                    self.accum = 0.0;
                    self.playing = false;
                    self.audio.stop();
                    self.mark_physics_reset();
                    self.run_all(true, 0.0);
                    self.message = format!("{name} を選択中のモデルに適用しました");
                }
            }
            LoadEvent::Motion(Err(error)) => {
                self.message = format!("モーションを読めませんでした: {error}");
            }
            LoadEvent::Audio(Ok((data, label))) => match self.audio.store(data, label) {
                Ok(()) => self.message = format!("{} を読み込みました", self.audio.label()),
                Err(error) => self.message = error,
            },
            LoadEvent::Audio(Err(error)) => self.message = error,
        }
    }

    /// The viewport should keep redrawing while a clip is playing, loading, or settling.
    pub fn needs_redraw(&self) -> bool {
        self.playing
            || self.loading
            || (self.physics_enabled && self.actors.iter().any(|actor| !actor.physics_settled))
    }

    /// The vertex buffers now match the evaluated positions.
    pub fn clear_geometry_dirty(&mut self) {
        for actor in &mut self.actors {
            actor.geometry_dirty = false;
        }
    }

    fn start_load(&mut self, kind: OpenFileKind, path: PathBuf) {
        let (sender, receiver) = mpsc::channel();
        self.incoming = Some(receiver);
        self.loading = true;
        self.message = "読み込み中です".to_string();
        std::thread::spawn(move || {
            let event = match kind {
                OpenFileKind::Model => LoadEvent::Model(load_model_bundle(&path)),
                OpenFileKind::Motion => {
                    LoadEvent::Motion(load_motion(&path).map_err(|error| error.to_string()))
                }
                OpenFileKind::Audio => LoadEvent::Audio(StageAudio::decode(&path)),
            };
            let _ = sender.send(event);
        });
    }

    fn install_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sample_count: u32,
        loaded: LoadedModel,
    ) {
        let texture_uploads: Vec<ImageUpload<'_>> = loaded
            .textures
            .iter()
            .map(|texture| ImageUpload {
                width: texture.width,
                height: texture.height,
                rgba: &texture.rgba,
                extra_mips: &texture.mips,
            })
            .collect();
        let toon_uploads: Vec<ImageUpload<'_>> = loaded
            .toons
            .iter()
            .map(|texture| ImageUpload {
                width: texture.width,
                height: texture.height,
                rgba: &texture.rgba,
                extra_mips: &texture.mips,
            })
            .collect();
        match SkinnedModel::upload(
            device,
            queue,
            &loaded.asset,
            &texture_uploads,
            &toon_uploads,
            sample_count,
        ) {
            Ok(gpu) => {
                let parents = mmd_animation::external_parent_names(&loaded.asset);
                if !parents.is_empty() {
                    tracing::warn!(bones = ?parents, "external parent bones are not evaluated");
                }
                tracing::info!(
                    rigid_bodies = loaded.asset.rigid_bodies.len(),
                    joints = loaded.asset.joints.len(),
                    "Bullet will simulate this model"
                );
                let name = if loaded.asset.name.is_empty() {
                    "モデル".to_string()
                } else {
                    loaded.asset.name.clone()
                };
                let mut actor = Actor::new(
                    Arc::new(loaded.asset),
                    gpu,
                    loaded.path.display().to_string(),
                );
                actor.request(Some(self.frame), 0.0, false, self.physics_enabled);
                actor.collect();
                self.actors.push(actor);
                self.selected = self.actors.len() - 1;
                self.info_open = true;
                self.message = format!("{name} を読み込みました");
            }
            Err(error) => {
                self.message = format!("モデルを GPU に送れませんでした: {error}");
            }
        }
    }

    /// Ask every model for its pose, then wait for all of them.
    ///
    /// `evaluate` re-evaluates the current frame; `elapsed` is the physics time.
    fn run_all(&mut self, evaluate: bool, elapsed: f32) {
        let frame = evaluate.then_some(self.frame);
        let playing = self.playing;
        let enabled = self.physics_enabled;
        for actor in &mut self.actors {
            actor.drop_prefetch();
            actor.request(frame, elapsed, playing, enabled);
        }
        for actor in &mut self.actors {
            actor.collect();
        }
    }

    /// Show the playback frame `self.frame`, then start on the one after it.
    fn play_frame(&mut self, elapsed: f32) {
        let frame = self.frame;
        let enabled = self.physics_enabled;
        for actor in &mut self.actors {
            if actor.prefetched != Some(frame) {
                actor.drop_prefetch();
                actor.request(Some(frame), elapsed, true, enabled);
            }
        }
        for actor in &mut self.actors {
            actor.prefetched = None;
            actor.collect();
        }
        let next = frame + 1;
        if next <= self.last_frame() {
            for actor in &mut self.actors {
                actor.request(Some(next), elapsed, true, enabled);
                actor.prefetched = Some(next);
            }
        }
    }

    fn mark_physics_reset(&mut self) {
        for actor in &mut self.actors {
            actor.drop_prefetch();
            actor.mark_physics_reset();
        }
    }
}

impl Actor {
    fn new(model: Arc<ModelAsset>, gpu: SkinnedModel, path: String) -> Self {
        let worker = PoseWorker::spawn(Arc::clone(&model));
        Self {
            model,
            gpu,
            motion: None,
            matched_bones: 0,
            pose: None,
            worker,
            physics_reset: true,
            physics_settled: false,
            prefetched: None,
            geometry_dirty: true,
            path,
        }
    }

    fn set_motion(&mut self, motion: Option<Arc<Motion>>) {
        self.drop_prefetch();
        self.worker.send(PoseRequest::Motion(motion.clone()));
        self.motion = motion;
        self.matched_bones = self
            .motion
            .as_ref()
            .map(|motion| {
                motion
                    .bones
                    .iter()
                    .filter(|track| self.model.bone_index(&track.name).is_some())
                    .count()
            })
            .unwrap_or(0);
    }

    fn mark_physics_reset(&mut self) {
        self.physics_reset = true;
        self.physics_settled = false;
    }

    /// Wait for a playback request that is no longer wanted and discard it.
    fn drop_prefetch(&mut self) {
        if self.prefetched.take().is_some() {
            let _ = self.worker.receive();
        }
    }

    /// Start evaluating `frame` (when given) and stepping the world.
    ///
    /// A paused pose that has come to rest is not stepped again.
    fn request(&mut self, frame: Option<i32>, elapsed: f32, playing: bool, enabled: bool) {
        if frame.is_none() && self.physics_settled && enabled && !playing && !self.physics_reset {
            return;
        }
        self.worker.send(PoseRequest::Run {
            frame,
            elapsed,
            flags: SimulationFlags {
                enabled,
                playing,
                reset: self.physics_reset,
            },
        });
        self.physics_reset = false;
    }

    /// Wait for the answer to [`request`](Self::request) and show it.
    fn collect(&mut self) {
        let Some(reply) = self.worker.receive() else {
            return;
        };
        if let Some(frame) = reply.frame {
            let morphs_changed = self
                .pose
                .as_ref()
                .is_none_or(|pose| pose.morph_weights != frame.morph_weights);
            // Vertex positions, UVs, and material colors only change with a
            // morph. Re-sending every vertex each frame cost milliseconds.
            self.geometry_dirty |= morphs_changed;
            self.pose = Some(*frame);
            self.physics_settled = false;
        } else if let Some(pose) = self.pose.as_mut() {
            pose.bone_translation = reply.translation;
            pose.bone_rotation = reply.rotation;
            pose.skin = reply.skin;
        }
        self.physics_settled = reply.settled;
    }
}

impl PoseWorker {
    fn spawn(model: Arc<ModelAsset>) -> Self {
        let (requests, inbox) = mpsc::channel::<PoseRequest>();
        let (outbox, replies) = mpsc::channel::<PoseReply>();
        let thread = std::thread::Builder::new()
            .name("pose".to_string())
            .spawn(move || {
                let world = PhysicsWorld::new(&model);
                let mut state = PoseThread {
                    model,
                    motion: None,
                    world,
                    translation: Vec::new(),
                    rotation: Vec::new(),
                    skin: Vec::new(),
                };
                for request in inbox {
                    match request {
                        PoseRequest::Motion(motion) => state.motion = motion,
                        PoseRequest::Run {
                            frame,
                            elapsed,
                            flags,
                        } => {
                            let reply = state.run(frame, elapsed, flags);
                            if outbox.send(reply).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(error) => {
                tracing::error!(%error, "could not start a pose thread");
                None
            }
        };
        Self {
            requests: Some(requests),
            replies,
            pending: false,
            thread,
        }
    }

    fn send(&mut self, request: PoseRequest) {
        let expects_reply = matches!(request, PoseRequest::Run { .. });
        let Some(requests) = &self.requests else {
            return;
        };
        if requests.send(request).is_err() {
            tracing::error!("the pose thread stopped; the model keeps its last pose");
            self.requests = None;
            return;
        }
        self.pending |= expects_reply;
    }

    fn receive(&mut self) -> Option<PoseReply> {
        if !self.pending {
            return None;
        }
        self.pending = false;
        match self.replies.recv() {
            Ok(reply) => Some(reply),
            Err(_) => {
                tracing::error!("the pose thread stopped; the model keeps its last pose");
                self.requests = None;
                None
            }
        }
    }
}

impl Drop for PoseWorker {
    fn drop(&mut self) {
        // Closing the request channel ends the thread's loop.
        self.requests = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("a pose thread panicked");
        }
    }
}

impl PoseThread {
    fn run(&mut self, frame: Option<i32>, elapsed: f32, flags: SimulationFlags) -> PoseReply {
        let mut evaluated = frame.map(|frame| {
            let pose = evaluate(&self.model, self.motion.as_deref(), FrameTime::new(frame));
            self.translation.clone_from(&pose.bone_translation);
            self.rotation.clone_from(&pose.bone_rotation);
            self.skin.clone_from(&pose.skin);
            Box::new(pose)
        });
        let mut translation = self.translation.clone();
        let mut rotation = self.rotation.clone();
        let mut skin = self.skin.clone();
        self.world.simulate(
            &self.model,
            &mut translation,
            &mut rotation,
            &mut skin,
            flags,
            elapsed,
        );
        let settled = flags.enabled && !flags.playing && self.world.is_settled();
        match evaluated.as_mut() {
            Some(pose) => {
                pose.bone_translation = translation;
                pose.bone_rotation = rotation;
                pose.skin = skin;
                PoseReply {
                    frame: evaluated,
                    translation: Vec::new(),
                    rotation: Vec::new(),
                    skin: Vec::new(),
                    settled,
                }
            }
            None => PoseReply {
                frame: None,
                translation,
                rotation,
                skin,
                settled,
            },
        }
    }
}

/// Material values for one draw.
pub fn draw_materials(pose: &EvaluatedFrame) -> Vec<DrawMaterial> {
    pose.materials
        .iter()
        .map(|material| DrawMaterial {
            diffuse: material.diffuse,
            specular: material.specular,
            specular_power: material.specular_power,
            ambient: material.ambient,
            edge_color: material.edge_color,
            edge_size: material.edge_size,
            texture_factor: material.texture,
            sphere_factor: material.sphere,
            double_sided: material.double_sided,
            ground_shadow: material.ground_shadow,
            cast_shadow: material.cast_shadow,
            receive_shadow: material.receive_shadow,
            draw_edge: material.draw_edge,
        })
        .collect()
}

fn load_model_bundle(path: &std::path::Path) -> Result<LoadedModel, String> {
    let asset = load_model(path).map_err(|error| error.to_string())?;
    let textures = load_model_textures(path, &asset);
    let toons = load_shared_toons(path, &asset);
    Ok(LoadedModel {
        asset,
        textures,
        toons,
        path: path.to_path_buf(),
    })
}
