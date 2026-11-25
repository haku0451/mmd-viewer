//! Integer frame time.
//!
//! MMD motions are stored at 30 frames per second. Playback advances by whole
//! frames so evaluation stays on the same timeline the file uses.

/// One frame on the 30 fps MMD timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameTime(i32);

impl FrameTime {
    pub const ZERO: Self = Self(0);

    pub const fn new(frame: i32) -> Self {
        Self(frame)
    }

    /// The frame number stored in VMD keys.
    pub const fn frame(self) -> i32 {
        self.0
    }

    /// Move by a whole number of frames without wrapping past `i32`.
    pub fn saturating_add(self, frames: i32) -> Self {
        Self(self.0.saturating_add(frames))
    }
}

/// MMD's fixed timeline rate. The value is not stored in VMD files.
pub const FRAMES_PER_SECOND: i32 = 30;
