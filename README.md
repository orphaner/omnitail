# MTR - File Tail Viewer with Ratatui

This is a Rust application that displays the tail of a file in a beautiful terminal user interface using [Ratatui](https://ratatui.rs).

## Features

- Real-time file monitoring: Automatically updates when the file changes
- Beautiful TUI interface with Ratatui
- Graceful error handling
- Simple navigation controls

## Dependencies

- Rust (latest stable version)
- Cargo

## How to Build and Run

```bash
# Clone the repository
git clone <repository-url>
cd mtr

# Build the project
cargo build

# Run the application
cargo run
```

## Implementation Details

The application consists of:

1. `src/main.rs`: Entry point that initializes the TUI application with a list of files to monitor
2. `src/tui_v1.rs`: Contains the Ratatui-based file tail viewer implementation that integrates with the v1.rs file reading mechanism
3. `src/v1.rs`: Contains the original file watching and reading logic
4. `_geek/*.txt`: Sample files to monitor (can be changed)

The TUI implementation includes:
- A header with the application title
- A vertically split content area showing multiple files simultaneously
- A footer with help information
- File watching using the `notify` crate
- Integration with the v1.rs file reading mechanism
- Error handling for terminal operations
- Flexible layout that adapts to the number of files

The key features of this implementation:
- Multi-file support with vertical splitting
- Each file gets its own section in the display
- Real-time monitoring of all files
- Uses the same file reading mechanism as v1.rs for consistency
- Beautiful Ratatui interface with clear file boundaries

## Controls

- `q` or `ESC`: Quit the application
- `Ctrl+C`: Quit the application

## Configuration

The application currently monitors the `_geek/small.txt` file. To monitor a different file, modify the `main.rs` file.

## Implementation Details

The application consists of:

1. `src/main.rs`: Entry point that initializes the TUI application
2. `src/tui.rs`: Contains the Ratatui-based file tail viewer implementation
3. `_geek/small.txt`: Sample file to monitor (can be changed)

The TUI implementation includes:
- A header with the application title
- A scrollable content area showing the file contents
- A footer with help information
- File watching using the `notify` crate
- Error handling for terminal operations

## Troubleshooting

If you encounter "Device not configured" errors, this typically means the terminal doesn't support raw mode. The application will gracefully exit in this case.
