use super::{AllocationType, FreeListAllocator};
use crate::visualizer::{ColorScheme, SubAllocatorVisualizer};

impl SubAllocatorVisualizer for FreeListAllocator {
    fn supports_visualization(&self) -> bool {
        true
    }

    fn draw_base_info(&self, ui: &imgui::Ui) {
        ui.text("free list sub-allocator");
        ui.text(format!("chunk count: {}", self.chunks.len()));
        ui.text(format!("chunk id counter: {}", self.chunk_id_counter));
    }

    fn draw_visualization(
        &self,
        color_scheme: &ColorScheme,
        ui: &imgui::Ui,
        bytes_per_unit: i32,
        show_backtraces: bool,
    ) {
        let draw_list = ui.get_window_draw_list();
        let window_size = ui.window_size();
        let base_pos = ui.cursor_screen_pos();
        const LINE_HEIGHT: f32 = 10.0f32;
        const LINE_SPACING: f32 = 1.0f32;
        // Variables for keeping track of our own cursor.
        let mut line_x = 0.0f32;
        let mut line_y = 0.0f32;
        let line_width = window_size[0];
        struct LineMarker {
            x: f32,
            y: f32,
        }
        let mut line_markers = Vec::<LineMarker>::default();

        let mut sorted_chunks = self.chunks.values().collect::<Vec<_>>();
        sorted_chunks.sort_by(|a, b| a.offset.cmp(&b.offset));

        // Draw each chunk in the memory block.
        for chunk in sorted_chunks.iter() {
            // Select a color based on the memory type.
            let color = match chunk.allocation_type {
                AllocationType::Free => color_scheme.free_color,
                AllocationType::Linear => color_scheme.linear_color,
                AllocationType::NonLinear => color_scheme.non_linear_color,
            };
            // Draw one or multiple bars based on the size of the chunk.
            let mut bytes_to_draw = chunk.size as f32;
            loop {
                // Calculate how large the block should be. We take in account the size of the chunk,
                // and the amount of space that is left on the line.
                let units_to_draw = bytes_to_draw as f32 / bytes_per_unit as f32;
                let units_left_on_line = line_width - line_x;
                let units_to_draw = units_to_draw.min(units_left_on_line);
                // Determine bounds of chunk line
                let top_left = [base_pos[0] + line_x, base_pos[1] + line_y];
                let bottom_right = [
                    base_pos[0] + line_x + units_to_draw,
                    base_pos[1] + line_y + LINE_HEIGHT,
                ];
                if ui.is_rect_visible(top_left, bottom_right) {
                    // Draw chunk line.
                    draw_list
                        .add_rect(top_left, bottom_right, color)
                        .filled(true)
                        .build();

                    // Show chunk information in a tool tip when hovering over the chunk.
                    if ui.is_mouse_hovering_rect(top_left, bottom_right) {
                        ui.tooltip(|| {
                            ui.text(format!("chunk_id: {}", chunk.chunk_id));
                            ui.text(format!("size: 0x{:x}", chunk.size));
                            ui.text(format!("offset: 0x{:x}", chunk.offset));
                            ui.text(format!("allocation_type: {:?}", chunk.allocation_type));
                            if let Some(name) = &chunk.name {
                                ui.text(format!("name: {:?}", name));
                            }
                            if show_backtraces {
                                if let Some(backtrace) = &chunk.backtrace {
                                    ui.text(format!("backtrace: {:}", backtrace));
                                }
                            }
                        })
                    }
                }
                // Advance line counter.
                line_x += units_to_draw;
                // Go to next line if it reached the end.
                if line_x >= line_width {
                    line_x = 0.0f32;
                    line_y += LINE_HEIGHT + LINE_SPACING;
                }
                // Calculate how many bytes have been drawn, and subtract that from the number of bytes left to draw
                let bytes_drawn = units_to_draw as f32 * bytes_per_unit as f32;
                bytes_to_draw -= bytes_drawn;
                // Exit when there are no more bytes to draw.
                if bytes_to_draw < 1.0f32 {
                    // Add a line marker to the end of the chunk.
                    line_markers.push(LineMarker {
                        x: bottom_right[0],
                        y: top_left[1],
                    });
                    // Exit the loop.
                    break;
                }
            }
        }
        // Draw the line markers after drawing all the chunks, so that chunks don't overlap the line markers
        for line_marker in line_markers.iter() {
            let top_left = [line_marker.x, line_marker.y];
            let bottom_right = [line_marker.x, line_marker.y + LINE_HEIGHT];

            if ui.is_rect_visible(top_left, bottom_right) {
                // Draw a line to mark the end of the chunk.
                draw_list
                    .add_line(top_left, bottom_right, 0xffff_ffff)
                    .thickness(1.0f32)
                    .build();
            }
        }
        // Let ImGui know how much we drew using the draw list.
        ui.set_cursor_pos([line_x, line_y + LINE_HEIGHT]);
    }
}
