#![allow(clippy::new_without_default)]

use crate::dedicated_block_allocator;
use crate::free_list_allocator;
use crate::VulkanAllocator;
use crate::*;

use imgui::*;

// Default value for block visualizer granularity.
const DEFAULT_BYTES_PER_UNIT: i32 = 1024;

#[derive(Clone)]
pub struct ColorScheme {
    free_color: ImColor32,
    linear_color: ImColor32,
    non_linear_color: ImColor32,
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

pub(crate) trait SubAllocatorVisualizer {
    fn supports_visualization(&self) -> bool {
        false
    }
    fn draw_base_info(&self, ui: &imgui::Ui) {
        ui.text("No sub allocator information available");
    }
    fn draw_visualization(
        &self,
        _color_scheme: &ColorScheme,
        _ui: &imgui::Ui,
        _bytes_per_unit: i32,
        _show_backtraces: bool,
    ) {
    }
}

impl SubAllocatorVisualizer for free_list_allocator::FreeListAllocator {
    fn supports_visualization(&self) -> bool {
        true
    }

    fn draw_base_info(&self, ui: &imgui::Ui) {
        ui.text("free list sub-allocator");
        ui.text(&format!("chunk count: {}", self.chunks.len()));
        ui.text(&format!("chunk id counter: {}", self.chunk_id_counter));
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
                            ui.text(&format!("chunk_id: {}", chunk.chunk_id));
                            ui.text(&format!("size: 0x{:x}", chunk.size));
                            ui.text(&format!("offset: 0x{:x}", chunk.offset));
                            ui.text(&format!("allocation_type: {:?}", chunk.allocation_type));
                            if let Some(name) = &chunk.name {
                                ui.text(&format!("name: {:?}", name));
                            }
                            if show_backtraces {
                                if let Some(backtrace) = &chunk.backtrace {
                                    ui.text(&format!("backtrace: {:}", backtrace));
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
impl SubAllocatorVisualizer for dedicated_block_allocator::DedicatedBlockAllocator {
    fn draw_base_info(&self, ui: &imgui::Ui) {
        ui.text("Dedicated Block");
    }
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

    fn render_main_window(&mut self, ui: &imgui::Ui, alloc: &VulkanAllocator) {
        imgui::Window::new(imgui::im_str!("Allocator visualization"))
            .collapsed(true, Condition::FirstUseEver)
            .size([512.0, 512.0], imgui::Condition::FirstUseEver)
            .build(ui, || {
                use imgui::*;

                ui.text(&format!(
                    "buffer image granularity: {:?}",
                    alloc.buffer_image_granularity
                ));

                let heap_count = alloc.memory_heaps.len();
                if CollapsingHeader::new(&im_str!("Memory Heaps ({} heaps)", heap_count)).build(ui)
                {
                    for (i, heap) in alloc.memory_heaps.iter().enumerate() {
                        ui.indent();
                        if CollapsingHeader::new(&im_str!("Heap: {}", i)).build(ui) {
                            ui.indent();
                            ui.text(&format!(
                                "flags: {:?} (0x{:x})",
                                heap.flags,
                                heap.flags.as_raw()
                            ));
                            ui.text(&format!(
                                "size:  {} MiB",
                                heap.size as f64 / (1024 * 1024) as f64
                            ));
                            ui.unindent();
                        }
                        ui.unindent();
                    }
                }

                if CollapsingHeader::new(&im_str!(
                    "Memory Types: ({} types)",
                    alloc.memory_types.len()
                ))
                .flags(TreeNodeFlags::DEFAULT_OPEN)
                .build(ui)
                {
                    ui.indent();
                    for (mem_type_i, mem_type) in alloc.memory_types.iter().enumerate() {
                        if CollapsingHeader::new(&im_str!(
                            "Type: {} ({} blocks)###Type{}",
                            mem_type_i,
                            mem_type.memory_blocks.len(),
                            mem_type_i
                        ))
                        .build(ui)
                        {
                            let mut total_block_size = 0;
                            let mut total_allocated = 0;
                            for block in mem_type.memory_blocks.iter().flatten() {
                                total_block_size += block.size;
                                total_allocated += block.sub_allocator.allocated();
                            }
                            ui.text(&format!(
                                "properties: {:?} (0x{:x})",
                                mem_type.memory_properties,
                                mem_type.memory_properties.as_raw()
                            ));
                            ui.text(&format!("heap index: {}", mem_type.heap_index));
                            ui.text(&format!(
                                "total block size: {} KiB",
                                total_block_size / 1024
                            ));
                            ui.text(&format!("total allocated:  {} KiB", total_allocated / 1024));

                            let active_block_count = mem_type
                                .memory_blocks
                                .iter()
                                .filter(|block| block.is_some())
                                .count();
                            ui.text(&format!("block count: {}", active_block_count));
                            for (block_i, block) in mem_type.memory_blocks.iter().enumerate() {
                                if let Some(block) = block {
                                    TreeNode::new(&im_str!(
                                        "Block: {}##memtype({})",
                                        block_i,
                                        mem_type_i
                                    ))
                                    .label(&im_str!("Block: {}", block_i))
                                    .build(ui, || {
                                        use ash::vk::Handle;
                                        ui.indent();
                                        ui.text(&format!("size: {} KiB", block.size / 1024));
                                        ui.text(&format!(
                                            "allocated: {} KiB",
                                            block.sub_allocator.allocated() / 1024
                                        ));
                                        ui.text(&format!(
                                            "vk device memory: 0x{:x}",
                                            block.device_memory.as_raw()
                                        ));
                                        ui.text(&format!(
                                            "mapped pointer: 0x{:x}",
                                            block.mapped_ptr as usize
                                        ));

                                        block.sub_allocator.draw_base_info(ui);

                                        if block.sub_allocator.supports_visualization() {
                                            let button_name = im_str!(
                                                "visualize##memtype({})block({})",
                                                mem_type_i,
                                                block_i
                                            );
                                            if ui.small_button(&button_name) {
                                                match self
                                                    .selected_blocks
                                                    .iter()
                                                    .enumerate()
                                                    .find_map(|(i, x)| {
                                                        if x.memory_type_index == mem_type_i
                                                            && x.block_index == block_i
                                                        {
                                                            Some((i, (x)))
                                                        } else {
                                                            None
                                                        }
                                                    }) {
                                                    Some(x) => self.focus = Some(x.0),
                                                    None => self.selected_blocks.push(
                                                        AllocatorVisualizerBlockWindow::new(
                                                            mem_type_i, block_i,
                                                        ),
                                                    ),
                                                }
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

    fn render_memory_block_windows(&mut self, ui: &imgui::Ui, alloc: &VulkanAllocator) {
        use imgui::*;
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
            imgui::Window::new(&imgui::im_str!(
                "Block Visualizer##memtype({})block({})",
                window.memory_type_index,
                window.block_index
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
                    ui.text(&format!(
                        "Memory type {}, Memory block {}, Block size: {} KiB",
                        window.memory_type_index,
                        window.block_index,
                        memblock.size / 1024
                    ));

                    if alloc.debug_settings.store_stack_traces {
                        ui.checkbox(im_str!("Show backtraces"), &mut window.show_backtraces);
                    }
                    // Slider for changing the 'zoom' level of the visualizer.
                    const BYTES_PER_UNIT_MIN: i32 = 1;
                    const BYTES_PER_UNIT_MAX: i32 = 1024 * 1024;
                    Drag::new(im_str!("Bytes per Pixel (zoom)"))
                        .range(BYTES_PER_UNIT_MIN..=BYTES_PER_UNIT_MAX)
                        .speed(10.0f32)
                        .build(ui, &mut window.bytes_per_unit);

                    // Imgui can actually modify this number to be out of bounds, so we will clamp manually.
                    window.bytes_per_unit = window
                        .bytes_per_unit
                        .min(BYTES_PER_UNIT_MAX)
                        .max(BYTES_PER_UNIT_MIN);

                    // Draw the visualization in a child window.
                    imgui::ChildWindow::new(&im_str!(
                        "Visualization Sub-window##memtype({})block({})",
                        window.memory_type_index,
                        window.block_index
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

    pub fn render(&mut self, allocator: &VulkanAllocator, ui: &imgui::Ui) {
        self.render_main_window(ui, allocator);
        self.render_memory_block_windows(ui, allocator);
    }
}
