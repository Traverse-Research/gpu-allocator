use imgui::*;
#[derive(Clone)]
pub struct ColorScheme {
    pub free_color: ImColor32,
    pub linear_color: ImColor32,
    pub non_linear_color: ImColor32,
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self {
            free_color: 0xff9f_9f9f.into(),       // gray
            linear_color: 0xfffa_ce5b.into(),     // blue
            non_linear_color: 0xffb8_a9fa.into(), // pink
        }
    }
}

pub(crate) trait SubAllocatorVisualizer {
    fn supports_visualization(&self) -> bool {
        false
    }
    fn draw_base_info(&self, ui: &Ui<'_>) {
        ui.text("No sub allocator information available");
    }
    fn draw_visualization(
        &self,
        _color_scheme: &ColorScheme,
        _ui: &Ui<'_>,
        _bytes_per_unit: i32,
        _show_backtraces: bool,
    ) {
    }
}
