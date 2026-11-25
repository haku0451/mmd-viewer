//! CPU geometry for the editor reference grid.
//!
//! The grid lies on the internal XZ plane. +X is red, +Y is green, and +Z is
//! blue. These colors identify the editor basis; they are not an imported model.

/// One line endpoint uploaded to the grid vertex buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GridVertex {
    /// Position in internal world space.
    pub position: [f32; 3],
    /// Unlit RGB color.
    pub color: [f32; 3],
}

pub const GRID_SHADER: &str = include_str!("../../../assets/shaders/editor_grid.wgsl");

const GRID_HALF_EXTENT: i32 = 10;
const GRID_SPACING: f32 = 1.0;
/// The floor squares and the axes are drawn this much larger than one model unit.
const GUIDE_SCALE: f32 = 1.7;
const X_AXIS: [f32; 3] = [0.86, 0.28, 0.28];
const Y_AXIS: [f32; 3] = [0.30, 0.78, 0.38];
const Z_AXIS: [f32; 3] = [0.32, 0.48, 0.90];
const MAJOR: [f32; 3] = [0.30, 0.32, 0.36];
const MINOR: [f32; 3] = [0.18, 0.19, 0.22];

/// Floor lines, then the three axes, so each range can be drawn on its own.
pub struct EditorGrid {
    /// Line-list vertices. Floor lines come first.
    pub vertices: Vec<GridVertex>,
    /// Vertex count of the floor squares, not including the axes.
    pub floor_count: u32,
    /// Vertex count of the X, Y, and Z axes.
    pub axis_count: u32,
}

/// Build the reference grid as a line list.
pub fn editor_grid() -> EditorGrid {
    let mut floor = Vec::new();
    let mut axes = Vec::new();
    let spacing = GRID_SPACING * GUIDE_SCALE;
    let extent = GRID_HALF_EXTENT as f32 * spacing;
    for index in -GRID_HALF_EXTENT..=GRID_HALF_EXTENT {
        let position = index as f32 * spacing;
        let color = if index % 5 == 0 { MAJOR } else { MINOR };
        if index == 0 {
            push_line(&mut axes, [-extent, 0.0, 0.0], [extent, 0.0, 0.0], X_AXIS);
            push_line(&mut axes, [0.0, 0.0, -extent], [0.0, 0.0, extent], Z_AXIS);
            continue;
        }
        push_line(
            &mut floor,
            [-extent, 0.0, position],
            [extent, 0.0, position],
            color,
        );
        push_line(
            &mut floor,
            [position, 0.0, -extent],
            [position, 0.0, extent],
            color,
        );
    }
    push_line(&mut axes, [0.0, 0.0, 0.0], [0.0, extent, 0.0], Y_AXIS);
    let floor_count = floor.len() as u32;
    let axis_count = axes.len() as u32;
    floor.extend(axes);
    EditorGrid {
        vertices: floor,
        floor_count,
        axis_count,
    }
}

fn push_line(vertices: &mut Vec<GridVertex>, start: [f32; 3], end: [f32; 3], color: [f32; 3]) {
    vertices.push(GridVertex {
        position: start,
        color,
    });
    vertices.push(GridVertex {
        position: end,
        color,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_is_a_line_list_of_finite_positions() {
        let vertices = editor_grid().vertices;
        assert_eq!(vertices.len() % 2, 0);
        assert!(vertices.iter().all(|vertex| {
            vertex.position.iter().all(|value| value.is_finite())
                && vertex.color.iter().all(|value| value.is_finite())
        }));
    }

    #[test]
    fn colored_axes_reach_the_positive_editor_basis() {
        let reach = GRID_HALF_EXTENT as f32 * GRID_SPACING * GUIDE_SCALE;
        let vertices = editor_grid().vertices;
        assert!(
            vertices
                .iter()
                .any(|vertex| { vertex.position == [reach, 0.0, 0.0] && vertex.color == X_AXIS })
        );
        assert!(
            vertices
                .iter()
                .any(|vertex| vertex.position == [0.0, reach, 0.0] && vertex.color == Y_AXIS)
        );
        assert!(
            vertices
                .iter()
                .any(|vertex| { vertex.position == [0.0, 0.0, reach] && vertex.color == Z_AXIS })
        );
    }

    #[test]
    fn grid_shader_exposes_the_vertex_and_fragment_entries() {
        assert!(GRID_SHADER.contains("fn vs_main"));
        assert!(GRID_SHADER.contains("fn fs_main"));
        assert!(GRID_SHADER.contains("clip_from_world"));
    }
}
