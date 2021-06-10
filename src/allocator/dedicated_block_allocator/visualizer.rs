use super::DedicatedBlockAllocator;
use crate::visualizer::SubAllocatorVisualizer;

impl SubAllocatorVisualizer for DedicatedBlockAllocator {
    fn draw_base_info(&self, ui: &imgui::Ui) {
        ui.text("Dedicated Block");
    }
}
