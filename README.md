# AFC Finder

A simple, cross-platform GUI file explorer for iOS devices, built with Rust.

![screenshot](https://github.com/jkcoxson/afc_finder/blob/master/ss.png?raw=true)

AFC Finder is a desktop utility that provides a graphical interface for the Apple File Conduit (AFC) protocol. This allows you to browse the filesystem of a connected iPhone or iPad, transfer files, and manage directories.

It is built entirely in Rust using `egui` for the interface and [`idevice`](https://github.com/jkcoxson/idevice) for device communication.

## Features

  * **Device Discovery:** Automatically detects and lists connected iOS devices by name.
  * **Multiple Connection Modes:**
      * **Root:** Access the full AFC jail.
      * **App Documents:** Browse the `/Documents` directory of a specific user application.
      * **App Container:** Browse the entire sandboxed container of a specific user application.
      * **Crash Reports:** Access the device's crash log directory.
  * **Application Lister:** Fetches and displays a searchable list of installed user applications to easily select a `bundle_id` for "Documents" or "Container" mode.
  * **File Operations:**
      * Download files and folders (recursively).
      * Upload files and folders (recursively).
      * Create new directories.
      * Delete files and directories.
  * **Modern UI:**
      * Intuitive file explorer with an editable path bar.
      * Sortable columns (name, size, modified date).
      * Progress bars for active file transfers.
      * Status bar for feedback on operations.
      * In-app log viewer for debugging.

## Dependencies

To connect to an iOS device, you must have the necessary Apple mobile device drivers installed on your system.

  * **macOS:** Comes pre-installed.
  * **Windows:** Installed with **iTunes** from Apple's website (not the Microsoft Store).
  * **Linux:** Requires `usbmuxd` to be installed and running.

## Building from Source

1.  Ensure you have the [Rust toolchain](https://rustup.rs/) installed.
2.  Clone the repository:
    ```sh
    git clone https://github.com/jkcoxson/afc_finder.git
    cd afc_finder
    ```
3.  Build and run the project:
    ```sh
    # For a debug build
    cargo run

    # For a release build
    cargo build --release
    ./target/release/afc_finder
    ```

## Technical Overview

The application uses a multi-threaded, asynchronous architecture:

  * The **`egui`** GUI runs on the main thread.
  * A **`tokio`** multi-thread runtime is spawned to handle all blocking I/O and device communication.
  * Separate `tokio` tasks are dedicated to:
    1.  Device discovery (`usbmuxd`, `lockdown`).
    2.  Application listing (`installation_proxy`).
    3.  File operations (`afc`, `house_arrest`, `crashreportcopymobile`).
  * Communication between the GUI and the async tasks is managed using `tokio::sync::mpsc::unbounded_channel`s, following a command/event pattern.

### Core Technologies

  * **`eframe`** / **`egui`**: For the cross-platform GUI.
  * **`tokio`**: Asynchronous runtime.
  * [**`idevice`**](https://github.com/jkcoxson/idevice): For all iOS device communication (AFC, Lockdown, etc.).
  * **`rfd`**: (Rust File Dialog) For native "Open/Save" dialogs.
  * **`egui_logger`**: For the in-app log viewer.

## License

MIT
