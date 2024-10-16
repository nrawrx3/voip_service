use rustyline::Editor;  // Import Rustyline for REPL
use rustyline::history::FileHistory;  // For using file-based history

// The REPL function.
pub async fn start_repl() {
    // Create a Rustyline editor (REPL) with FileHistory for history handling.
    let mut rl = Editor::<(), FileHistory>::new().unwrap();
    println!("Starting REPL. Type 'help' for commands.");

    loop {
        // Read a line from the user.
        let readline = rl.readline(">> ");
        match readline {
            Ok(line) => {
                rl.add_history_entry(line.as_str()).unwrap();

                match line.as_str() {
                    "help" => {
                        println!("Available commands: help, status, exit");
                    }
                    "status" => {
                        println!("Server running. Status: OK.");
                    }
                    "exit" => {
                        println!("Exiting REPL.");
                        break;
                    }
                    _ => {
                        println!("Unknown command: {}", line);
                    }
                }
            }
            Err(e) => {
                // If EOF is encountered, exit the REPL.
                if matches!(e, rustyline::error::ReadlineError::Eof) {
                    println!("Exiting REPL.");
                    break;
                }

                println!("Error reading input. Exiting REPL.");
                break;
            }
        }
    }
}
