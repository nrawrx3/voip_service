use crate::MyVoipService;
use log::info;
use prettytable::{row, Table};
use rustyline::history::FileHistory;
use rustyline::Editor;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::sync::Mutex; // Use Tokio's Mutex

pub async fn start_repl(voip_service: Arc<Mutex<MyVoipService>>, shutdown_tx: oneshot::Sender<()>) {
    let mut rl = Editor::<(), FileHistory>::new().unwrap();
    println!("Starting REPL. Type 'help' for commands.");

    loop {
        let readline = rl.readline(">> ");
        match readline {
            Ok(line) => {
                rl.add_history_entry(line.as_str()).unwrap();

                let mut parts = line.split_whitespace();
                let command = parts.next().unwrap_or("");

                match command {
                    "help" => {
                        println!("Available commands: help, status, login, connect, exit");
                    }
                    "status" => {
                        let service = voip_service.lock().await;
                        let user_name = service.current_user_name.as_deref().unwrap_or("None");
                        let room_name = service.current_room_name.as_deref().unwrap_or("None");
                        println!(
                            "Current user: {}, Room: {}, Call state: {:?}, Have token: {}",
                            user_name,
                            room_name,
                            service.call_state,
                            service.livekit_token.is_some()
                        );
                    }
                    "login" => {
                        let user_name = parts.next();
                        let room_name = parts.next();

                        if let (Some(user), Some(room)) = (user_name, room_name) {
                            let mut service = voip_service.lock().await;
                            service.current_user_name = Some(user.to_string());
                            service.current_room_name = Some(room.to_string());
                            // println!("Logged in as {} in room {}", user, room);
                            info!("Calling connect_to_livekit");
                            service.connect_to_livekit().await;
                        } else {
                            println!("Usage: login <username> <room>");
                        }
                    }
                    "connect" => {
                        let service = voip_service.clone();
                        tokio::spawn(async move {
                            let mut service = service.lock().await;
                            if service.current_user_name.is_none()
                                || service.current_room_name.is_none()
                            {
                                println!("Please log in first using the 'login' command.");
                                return;
                            }

                            if let Err(e) = service.connect_to_livekit().await {
                                println!("Failed to connect: {}", e);
                            }
                        });
                    }
                    "current_room_info" => {
                        let service = voip_service.lock().await;
                        match service.current_room_info().await {
                            Ok(room_info) => {
                                let mut table = Table::new();

                                // Add title row for local participant
                                table.add_row(row![
                                    "Local Participant",
                                    "Is Speaking",
                                    "Audio Level",
                                    "Kind"
                                ]);
                                let local = &room_info.local_participant;
                                table.add_row(row![
                                    local.identity,
                                    local.is_speaking.to_string(),
                                    format!("{:.2}", local.audio_level),
                                    format!("{:?}", local.kind),
                                ]);

                                // Add title row for remote participants
                                table.add_row(row![
                                    "Remote Participants",
                                    "Is Speaking",
                                    "Audio Level",
                                    "Kind"
                                ]);
                                for remote in &room_info.remote_participants {
                                    table.add_row(row![
                                        remote.identity,
                                        remote.is_speaking.to_string(),
                                        format!("{:.2}", remote.audio_level),
                                        format!("{:?}", remote.kind),
                                    ]);
                                }

                                table.printstd();
                            }
                            Err(e) => {
                                println!("Error: {}", e);
                            }
                        }
                    }
                    "exit" => {
                        println!("Exiting REPL and shutting down gRPC server.");
                        let _ = shutdown_tx.send(());
                        break;
                    }
                    _ => {
                        println!("Unknown command: {}", line);
                    }
                }
            }
            Err(e) => {
                if matches!(e, rustyline::error::ReadlineError::Eof) {
                    println!("Exiting REPL and shutting down gRPC server.");
                    let _ = shutdown_tx.send(());
                    break;
                }
                println!("Error reading input. Exiting REPL.");
                break;
            }
        }
    }
}
