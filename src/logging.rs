use env_logger::{Builder, Target};
use std::fs::File;
use std::io::{self, Write};
use std::sync::Mutex;

// Define a struct that wraps Mutex<File> and implements Write
struct MutexWriter {
    inner: Mutex<File>,
}

impl MutexWriter {
    fn new(file: File) -> Self {
        Self {
            inner: Mutex::new(file),
        }
    }
}

impl Write for MutexWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut file = self.inner.lock().unwrap();
        file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = self.inner.lock().unwrap();
        file.flush()
    }
}

pub fn setup_logging() -> Result<(), Box<dyn std::error::Error>> {
    let log_file = File::create("C:\\voip_service.log")?; // Specify the log file path here
    let writer = MutexWriter::new(log_file); // Wrap the File in MutexWriter

    Builder::new()
        .target(Target::Pipe(Box::new(writer))) // Use the custom writer here
        .format(|buf, record| writeln!(buf, "[{}] - {}", record.level(), record.args()))
        .filter(None, log::LevelFilter::Info)
        .init();

    Ok(())
}
