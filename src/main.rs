#![no_std]
#![no_main]

use alloc::vec;
use embedded_graphics::pixelcolor::Rgb565;
use linked_list_allocator::LockedHeap;
use log::info;
extern crate alloc;
mod tinygif;
mod minipng;
mod logger;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

use core::{ffi::c_int, panic::PanicInfo};

/// 自定义 panic handler
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // 如果你想打印 panic 信息，可以在这里实现
    loop {
        // 进入无限循环，停止执行
    }
}

#[no_mangle]
pub extern "C" fn main() -> c_int{
    // Initialize the allocator BEFORE you use it
    {
        const HEAP_SIZE: usize = 1024*2048;
        static mut HEAP_MEM: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];
        unsafe {
            ALLOCATOR.lock().init(HEAP_MEM.as_mut_ptr(), HEAP_SIZE);
        }
    }
    logger::init();
    info!("hello!");
    test_gif();
    test_png();
    return 0;
}

fn test_gif(){
    let image = tinygif::Gif::<Rgb565>::from_slice(include_bytes!("../2.gif")).unwrap();
    let frame = image.frames().next().unwrap();
    let rgb = frame.decode_to_rgb().unwrap();
    info!("gif rgb:{}", rgb.len());
}

fn test_png(){
    let png = include_bytes!("../2.png").to_vec();
	let header = minipng::decode_png_header(&png).expect("bad PNG");
	info!("png need {} bytes of memory", header.required_bytes());
	let mut buffer = vec![0; header.required_bytes()];
	let image = minipng::decode_png(&png, &mut buffer).expect("bad PNG");
    info!("png {}x{} image", image.width(), image.height());
}
