use ash::version::DeviceV1_0;
use ash::vk;

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_and_submit_command_buffer<D: DeviceV1_0, F: FnOnce(&D, vk::CommandBuffer)>(
    device: &D,
    command_buffer: vk::CommandBuffer,
    command_buffer_reuse_fence: vk::Fence,
    submit_queue: vk::Queue,
    wait_mask: &[vk::PipelineStageFlags],
    wait_semaphores: &[vk::Semaphore],
    signal_semaphores: &[vk::Semaphore],
    f: F,
) {
    unsafe { device.wait_for_fences(&[command_buffer_reuse_fence], true, std::u64::MAX) }.unwrap();
    unsafe { device.reset_fences(&[command_buffer_reuse_fence]) }.unwrap();
    unsafe {
        device.reset_command_buffer(
            command_buffer,
            vk::CommandBufferResetFlags::RELEASE_RESOURCES,
        )
    }
    .unwrap();

    let command_buffer_begin_info =
        vk::CommandBufferBeginInfo::builder().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    unsafe { device.begin_command_buffer(command_buffer, &command_buffer_begin_info) }.unwrap();

    f(device, command_buffer);

    unsafe { device.end_command_buffer(command_buffer) }.unwrap();

    let command_buffers = [command_buffer];
    let submit_info = vk::SubmitInfo::builder()
        .wait_semaphores(wait_semaphores)
        .wait_dst_stage_mask(wait_mask)
        .command_buffers(&command_buffers)
        .signal_semaphores(signal_semaphores);

    unsafe {
        device.queue_submit(
            submit_queue,
            std::slice::from_ref(&submit_info),
            command_buffer_reuse_fence,
        )
    }
    .unwrap();
}
