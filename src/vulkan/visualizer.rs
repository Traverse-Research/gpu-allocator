#![allow(clippy::new_without_default)]

use super::Allocator;
use crate::visualizer::ColorScheme;

// Default value for block visualizer granularity.
const DEFAULT_BYTES_PER_UNIT: i32 = 1024;

struct AllocatorVisualizerBlockWindow {
    memory_type_index: usize,
    block_index: usize,
    bytes_per_unit: i32,
    show_backtraces: bool,
}
impl AllocatorVisualizerBlockWindow {
    fn new(memory_type_index: usize, block_index: usize) -> Self {
        Self {
            memory_type_index,
            block_index,
            bytes_per_unit: DEFAULT_BYTES_PER_UNIT,
            show_backtraces: false,
        }
    }
}
pub struct AllocatorVisualizer {
    selected_blocks: Vec<AllocatorVisualizerBlockWindow>,
    focus: Option<usize>,
    color_scheme: ColorScheme,
}

impl AllocatorVisualizer {
    pub fn new() -> Self {
        Self {
            selected_blocks: Vec::default(),
            focus: None,
            color_scheme: ColorScheme::default(),
        }
    }

    pub fn set_color_scheme(&mut self, color_scheme: ColorScheme) {
        self.color_scheme = color_scheme;
    }

    fn render_main_window(&mut self, ui: &imgui::Ui, opened: Option<&mut bool>, alloc: &Allocator) {
        let mut window = imgui::Window::new("Allocator visualization");

        if let Some(opened) = opened {
            window = window.opened(opened);
        }

        window
            .size([512.0, 512.0], imgui::Condition::FirstUseEver)
            .build(ui, || {
                use imgui::*;

                ui.text(format!(
                    "buffer image granularity: {:?}",
                    alloc.buffer_image_granularity
                ));

                let heap_count = alloc.memory_heaps.len();
                if CollapsingHeader::new(format!("Memory Heaps ({} heaps)", heap_count)).build(ui) {
                    for (i, heap) in alloc.memory_heaps.iter().enumerate() {
                        ui.indent();
                        if CollapsingHeader::new(format!("Heap: {}", i)).build(ui) {
                            ui.indent();
                            ui.text(format!("flags: {:?}", heap.flags));
                            ui.text(format!(
                                "size:  {} MiB",
                                heap.size as f64 / (1024 * 1024) as f64
                            ));
                            ui.unindent();
                        }
                        ui.unindent();
                    }
                }

                if CollapsingHeader::new(format!(
                    "Memory Types: ({} types)",
                    alloc.memory_types.len()
                ))
                .flags(TreeNodeFlags::DEFAULT_OPEN)
                .build(ui)
                {
                    ui.indent();
                    for (mem_type_i, mem_type) in alloc.memory_types.iter().enumerate() {
                        if CollapsingHeader::new(format!(
                            "Type: {} ({} blocks)##Type{}",
                            mem_type_i,
                            mem_type.memory_blocks.len(),
                            mem_type_i,
                        ))
                        .build(ui)
                        {
                            let mut total_block_size = 0;
                            let mut total_allocated = 0;
                            for block in mem_type.memory_blocks.iter().flatten() {
                                total_block_size += block.size;
                                total_allocated += block.sub_allocator.allocated();
                            }
                            ui.text(format!("properties: {:?}", mem_type.memory_properties));
                            ui.text(format!("heap index: {}", mem_type.heap_index));
                            ui.text(format!("total block size: {} KiB", total_block_size / 1024));
                            ui.text(format!("total allocated:  {} KiB", total_allocated / 1024));

                            let active_block_count = mem_type
                                .memory_blocks
                                .iter()
                                .filter(|block| block.is_some())
                                .count();
                            ui.text(format!("block count: {}", active_block_count));
                            for (block_i, block) in mem_type.memory_blocks.iter().enumerate() {
                                if let Some(block) = block {
                                    TreeNode::new(format!("Block: {}", block_i)).build(ui, || {
                                        use ash::vk::Handle;
                                        ui.indent();
                                        ui.text(format!("size: {} KiB", block.size / 1024));
                                        ui.text(format!(
                                            "allocated: {} KiB",
                                            block.sub_allocator.allocated() / 1024
                                        ));
                                        ui.text(format!(
                                            "vk device memory: 0x{:x}",
                                            block.device_memory.as_raw()
                                        ));
                                        ui.text(format!(
                                            "mapped pointer: 0x{:x}",
                                            block.mapped_ptr as usize
                                        ));

                                        block.sub_allocator.draw_base_info(ui);

                                        if block.sub_allocator.supports_visualization()
                                            && ui.small_button("visualize")
                                        {
                                            match self.selected_blocks.iter().enumerate().find(
                                                |(_, x)| {
                                                    x.memory_type_index == mem_type_i
                                                        && x.block_index == block_i
                                                },
                                            ) {
                                                Some(x) => self.focus = Some(x.0),
                                                None => self.selected_blocks.push(
                                                    AllocatorVisualizerBlockWindow::new(
                                                        mem_type_i, block_i,
                                                    ),
                                                ),
                                            }
                                        }
                                        ui.unindent();
                                    });
                                }
                            }
                        }
                    }
                    ui.unindent();
                }
            });
    }

    fn render_memory_block_windows(&mut self, ui: &imgui::Ui, alloc: &Allocator) {
        // Copy here to workaround the borrow checker.
        let focus_opt = self.focus;
        // Keep track of a list of windows that are signaled by imgui to be closed.
        let mut windows_to_close = Vec::default();
        // Draw each window.
        let color_scheme = &self.color_scheme;
        for (window_i, window) in self.selected_blocks.iter_mut().enumerate() {
            // Determine if this window needs focus.
            let focus = if let Some(focus_i) = focus_opt {
                window_i == focus_i
            } else {
                false
            };
            let mut is_open = true;
            imgui::Window::new(format!(
                "Block Visualizer##memtype({})block({})",
                window.memory_type_index, window.block_index
            ))
            .size([1920.0 * 0.5, 1080.0 * 0.5], imgui::Condition::FirstUseEver)
            .title_bar(true)
            .scroll_bar(true)
            .scrollable(true)
            .focused(focus)
            .opened(&mut is_open)
            .build(ui, || {
                use imgui::*;

                let memblock = &alloc.memory_types[window.memory_type_index].memory_blocks
                    [window.block_index]
                    .as_ref();
                if let Some(memblock) = memblock {
                    ui.text(format!(
                        "Memory type {}, Memory block {}, Block size: {} KiB",
                        window.memory_type_index,
                        window.block_index,
                        memblock.size / 1024
                    ));

                    if alloc.debug_settings.store_stack_traces {
                        ui.checkbox("Show backtraces", &mut window.show_backtraces);
                    }
                    // Slider for changing the 'zoom' level of the visualizer.
                    const BYTES_PER_UNIT_MIN: i32 = 1;
                    const BYTES_PER_UNIT_MAX: i32 = 1024 * 1024;
                    Drag::new("Bytes per Pixel (zoom)")
                        .range(BYTES_PER_UNIT_MIN, BYTES_PER_UNIT_MAX)
                        .speed(10.0f32)
                        .build(ui, &mut window.bytes_per_unit);

                    // Imgui can actually modify this number to be out of bounds, so we will clamp manually.
                    window.bytes_per_unit = window
                        .bytes_per_unit
                        .min(BYTES_PER_UNIT_MAX)
                        .max(BYTES_PER_UNIT_MIN);

                    // Draw the visualization in a child window.
                    imgui::ChildWindow::new(&format!(
                        "Visualization Sub-window##memtype({})block({})",
                        window.memory_type_index, window.block_index
                    ))
                    .scrollable(true)
                    .scroll_bar(true)
                    .build(ui, || {
                        memblock.sub_allocator.draw_visualization(
                            color_scheme,
                            ui,
                            window.bytes_per_unit,
                            window.show_backtraces,
                        )
                    });
                } else {
                    ui.text("Deallocated memory block");
                }
            });
            // If imgui signalled to close the window, add it to the list of windows to close.
            if !is_open {
                windows_to_close.push(window_i);
            }
        }
        //
        // Clean-up
        //
        // Close windows.
        let mut windows_removed = 0usize;
        let mut i = 0usize;
        if !windows_to_close.is_empty() && !self.selected_blocks.is_empty() {
            loop {
                if windows_to_close.iter().any(|j| i == (*j - windows_removed)) {
                    self.selected_blocks.remove(i);
                    windows_removed += 1;
                } else {
                    i += 1;
                }
                if i == self.selected_blocks.len() {
                    break;
                }
            }
        }
        // Reset focus.
        self.focus = None;
    }

    /// Renders imgui widgets.
    /// 
    /// The `Option<&mut bool>` can be used control and track changes to the opened/closed status of the widget.
    /// Pass [`None`] if no control and readback information is required. This will always render the widget.
    /// When passing `Some(&mut bool)`:
    /// - If [`false`], the widget won't be drawn.
    /// - If [`true`], the widget will be drawn and an (X) closing button will be added to the widget bar.
    pub fn render(&mut self, allocator: &Allocator, ui: &imgui::Ui, opened: Option<&mut bool>) {
        if opened != Some(&mut false) {
            self.render_main_window(ui, opened, allocator);
            self.render_memory_block_windows(ui, allocator);
        }
    }
}
