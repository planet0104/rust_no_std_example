use alloc::{ffi::CString, format, string::String};
use log::{Level, LevelFilter, Metadata, Record};
use static_cell::StaticCell;

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }
    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let s = format!("{} - {}\r\n", record.level(), record.args());
            println(s);
        }
    }
    fn flush(&self) {}
}

pub fn init(){
    static LOGGER: StaticCell<SimpleLogger> = StaticCell::new();
    let logger_ptr = LOGGER.init(SimpleLogger {});
    unsafe{
        log::set_logger_racy(logger_ptr)
        .map(|()| log::set_max_level_racy(LevelFilter::Info)).unwrap();
    }
}

fn println(mut s:String){
    s.push_str("\r\n");
    let s = CString::new(s).unwrap();
    unsafe{ libc::printf(s.as_ptr()); }
}