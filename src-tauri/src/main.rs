#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // WebKitGTK's DMA-BUF buffers die with Wayland protocol error 71 on NVIDIA.
    // Shared memory hands over the frames instead and keeps GPU rendering.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_none() {
        // Safe because no other thread exists yet.
        unsafe { std::env::set_var("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1") };
    }
    ron_mod_manager_lib::run()
}
