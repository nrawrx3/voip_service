## Build

- Install rust toolchain using [rustup](https://rustup.rs/). Don't use brew.
  Make sure rustup's binaries are used, not brew's if you have installed using
  brew.

- If on windows install windows protoc from
  [github](https://github.com/protocolbuffers/protobuf/releases). On MacOS, just
  use brew.

### For MacOS

Seems `cargo` does not pick up instructions from config.toml on my mac. So this
extra arg needs to be passed as env variable.

```
export RUSTFLAGS="-C link-args=-ObjC"
```

### For Windows

Make sure to use powershell plz.

```
$env:RUSTFLAGS="-C target-feature=+crt-static --cfg tokio_unstable"
```

### Run the service (deprecated: See next section)
Then

```
export RUST_LOG=info
cargo build
cargo run
```

### Running as a windows service

- Use the `.\setup.ps1` script.
 
It uses `sc.exe` for creating, starting and stopping the service as follows:

- Build and ensure the debug exe is at `.\target\debug\voip_service.exe`
- Run `sc.exe create voipservice binPath= "<project-dir>\target\debug\voip_service.exe"` (Replace <project-dir> with path to checked out repo)
- Start the service with `sc.exe start voipservice`
- View the log file at `C:\voip_service.log`
- Stop the service with `sc.exe stop voipservice`
- Delete the service registration entirely with `sc.exe delete voipservice`

For example
```
sc.exe create voipservice binPath= "C:\Users\Soumik\werk\voip_service\target\debug\voip_service.exe"
```


## Testing with dummy events

The test livekit token/auth server hosted at `lktoken.nrawrx3.xyz` currently 1
hardcoded room and 3 hardcoded users. We will probably use our new backend for
token generation later if we stick to livekit.

- Install Go and [grpc-client-cli](https://github.com/vadimi/grpc-client-cli),
make sure the grpc-client-cli binary is in PATH (should be if you have set up
GOPATH correctly).

### Spawn a command line client

- Make sure you are in this project's root directory.
- In one terminal run `cargo run` - this is the voip_service itself. This listens for grpc requests on `:50051`

- Create another terminal and run `grpc-client-cli --deadline 1h --proto proto/voip_service_grpc.proto localhost:50051`
- Choose `voip.VoipService` as the rpc service.
- Choose `JoinRoom` as the rpc method.
- Enter the request
  `{"client_id":"frontend_app","user_name":"alice#123","room_name":"test-room-1"}`.
  This lets you join the test room as user `alice#123`. The `client_id` is
  `frontend_app` is used for keeping track of multiple clients. For now it
  doesn't matter. This client will now keep receiving events on the gRPC stream.


### Spawn another client for sending dummy events

- Create yet another terminal and run the same `grpc-client-cli --deadline 1h
  --proto proto/voip_service_grpc.proto localhost:50051`. This one is solely for
  sending events to client manually, like a debug console. Choose
  `SendDebugEventToClient` as the rpc method this time.
  
- Let's say we want to send the event of type
  `event_type_active_speakers_changed` to the `frontend_app` client to tell it
  that currently 2 members are speaking `alice#123` and `bob#456`. We send `{"client_id":"debug","dest_client_id":"frontend_app","event":{"event_type":"event_type_active_speakers_changed","current_user_name":"alice#123","current_room_name":"test-room-1","active_speakers_changed":{"active_speaker_ids":["alice#123","bob#456"]}}}`. The pretty JSON is

```json
  {
    "client_id": "debug",
    "dest_client_id": "frontend_app",
    "event": {
      "event_type": "event_type_active_speakers_changed",
      "current_user_name": "alice#123",
      "current_room_name": "test-room-1",
      "active_speakers_changed": {
        "active_speaker_ids": [
          "alice#123",
          "bob#456"
        ]
      }
    }
  }
```
