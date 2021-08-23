fn main() {
    windows::build! {
        Windows::Win32::Foundation::E_NOINTERFACE,
        Windows::Win32::Graphics::Direct3D12::*,
        Windows::Win32::Graphics::Dxgi::*,
    };
}
