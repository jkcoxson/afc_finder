// Jackson Coxson
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use egui::{Color32, ComboBox, TextEdit};
use log::error;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use idevice::{
    IdeviceError, IdeviceService,
    afc::{AfcClient, FileInfo, opcode::AfcFopenMode},
    crashreportcopymobile::CrashReportCopyMobileClient,
    house_arrest::HouseArrestClient,
    installation_proxy::InstallationProxyClient,
    lockdown::LockdownClient,
    usbmuxd::{UsbmuxdAddr, UsbmuxdConnection, UsbmuxdDevice},
};
use rfd::FileDialog;

fn main() {
    egui_logger::builder().init().unwrap();
    let (gui_sender, gui_recv) = unbounded_channel();
    let (idevice_sender, mut idevice_receiver) = unbounded_channel();
    let (afc_sender, mut afc_receiver) = unbounded_channel();
    let (app_sender, mut app_receiver) = unbounded_channel::<AppCommands>();

    // Start with an initial device scan
    idevice_sender.send(IdeviceCommands::GetDevices).unwrap();

    let self_afc_sender = afc_sender.clone();
    let app = MyApp {
        devices: None,
        devices_placeholder: "Loading...".to_string(),
        selected_device: "".to_string(),
        gui_recv,
        idevice_sender: idevice_sender.clone(),
        afc_sender,
        app_sender,
        show_logs: false,
        afc_state: AfcState::default(),
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Task for device discovery and basic info
    let gui_sender_dis = gui_sender.clone();
    rt.spawn(async move {
        while let Some(command) = idevice_receiver.recv().await {
            match command {
                IdeviceCommands::GetDevices => {
                    let mut uc = match UsbmuxdConnection::default().await {
                        Ok(u) => u,
                        Err(e) => {
                            gui_sender_dis.send(GuiCommands::NoUsbmuxd(e)).unwrap();
                            continue;
                        }
                    };

                    match uc.get_devices().await {
                        Ok(devs) => {
                            let mut selections = HashMap::new();
                            for dev in devs {
                                let p = dev.to_provider(UsbmuxdAddr::default(), "afc_gui");
                                let mut lc = match LockdownClient::connect(&p).await {
                                    Ok(l) => l,
                                    Err(e) => {
                                        error!("Failed to connect to lockdown: {e:?}");
                                        continue;
                                    }
                                };
                                let values = lc.get_value(None, None).await.unwrap();
                                let device_name = values
                                    .as_dictionary()
                                    .and_then(|x| x.get("DeviceName"))
                                    .and_then(|x| x.as_string())
                                    .unwrap()
                                    .to_string();
                                selections.insert(device_name, dev);
                            }

                            gui_sender_dis
                                .send(GuiCommands::Devices(selections))
                                .unwrap();
                        }
                        Err(e) => {
                            gui_sender_dis
                                .send(GuiCommands::GetDevicesFailure(e))
                                .unwrap();
                        }
                    }
                }
            }
        }
    });

    // Task for AFC operations
    let gui_sender_afc = gui_sender.clone();
    rt.spawn(async move {
        let mut afc_client: Option<AfcClient> = None;

        while let Some(command) = afc_receiver.recv().await {
            match command {
                AfcCommands::Connect(dev, afc_mode) => {
                    let provider = Arc::new(dev.to_provider(UsbmuxdAddr::default(), "afc_gui"));
                    let client_result = match afc_mode {
                        AfcMode::Root => AfcClient::connect(&*provider).await,
                        AfcMode::Documents(bundle_id) => {
                            match HouseArrestClient::connect(&*provider).await {
                                Ok(h) => h.vend_documents(&bundle_id).await,
                                Err(e) => Err(e),
                            }
                        }
                        AfcMode::Container(bundle_id) => {
                            match HouseArrestClient::connect(&*provider).await {
                                Ok(h) => h.vend_container(&bundle_id).await,
                                Err(e) => Err(e),
                            }
                        }
                        AfcMode::CrashReports => {
                            match CrashReportCopyMobileClient::connect(&*provider).await {
                                Ok(c) => Ok(c.to_afc_client()),
                                Err(e) => Err(e),
                            }
                        }
                    };

                    match client_result {
                        Ok(client) => {
                            afc_client = Some(client);
                            gui_sender_afc
                                .send(GuiCommands::Afc(GuiAfcCommands::ConnectionStatus(Ok(()))))
                                .unwrap();
                        }
                        Err(e) => {
                            gui_sender_afc
                                .send(GuiCommands::Afc(GuiAfcCommands::ConnectionStatus(Err(
                                    e.to_string()
                                ))))
                                .unwrap();
                        }
                    }
                }
                AfcCommands::ListDirectory(path) => {
                    if let Some(client) = &mut afc_client {
                        // Get the initial list of names
                        let items_result = match client.list_dir(&path).await {
                            Ok(items) => {
                                let mut detailed_items = Vec::new();
                                // Iterate and get info for each item
                                for item in items {
                                    if item == "." || item == ".." {
                                        continue;
                                    }
                                    // We need the full path to get info
                                    let full_path =
                                        Path::new(&path).join(&item).to_string_lossy().to_string();

                                    match client.get_file_info(&full_path).await {
                                        Ok(info) => {
                                            // Add the name and its info to our list
                                            detailed_items.push(AfcItem { name: item, info });
                                        }
                                        Err(e) => {
                                            // Log error and skip file if info fails
                                            error!("Failed to get info for {}: {:?}", full_path, e);
                                        }
                                    }
                                }
                                Ok(detailed_items)
                            }
                            Err(e) => Err(e.to_string()),
                        };

                        // Send the detailed list back to the GUI
                        gui_sender_afc
                            .send(GuiCommands::Afc(GuiAfcCommands::DirectoryListing(
                                items_result,
                            )))
                            .unwrap();
                    }
                }
                AfcCommands::DownloadFile(remote_path, local_path) => {
                    if let Some(client) = &mut afc_client {
                        let status = async {
                            let mut file = client.open(&remote_path, AfcFopenMode::RdOnly).await?;

                            // Clone sender and remote path for the callback
                            let sender = gui_sender_afc.clone();
                            let path_clone = remote_path.clone();

                            // Define the async callback (using bytes read / total bytes)
                            let callback = |((bytes_read, total), path): ((usize, usize), String)| {
                                let sender_clone = sender.clone();
                                async move {
                                    // Send progress update
                                    let _ = sender_clone.send(GuiCommands::Afc(
                                        GuiAfcCommands::DownloadProgress(path, bytes_read, total),
                                    ));
                                }
                            };

                            // Use hypothetical read_with_callback
                            // Pass total_size obtained earlier
                            let data = file.read_with_callback( callback, path_clone).await?;

                            // Write data to local file
                            tokio::fs::write(&local_path, data).await?;

                            Ok::<(), Box<dyn std::error::Error>>(())
                        }
                        .await;

                        let msg = match status {
                            Ok(_) => Ok(format!(
                                "Downloaded {} to {}",
                                Path::new(&remote_path).file_name().unwrap_or_default().to_string_lossy(),
                                local_path.display()
                            )),
                            Err(e) => Err(e.to_string()),
                        };

                        // Send final status (success or error)
                        // This will implicitly clear the progress bar in the UI
                        gui_sender_afc
                            .send(GuiCommands::Afc(GuiAfcCommands::OperationStatus(msg)))
                            .unwrap(); // Or handle error
                    }
                }
                AfcCommands::UploadFile(local_path, remote_path) => {
                    if let Some(client) = &mut afc_client {
                        let status = async {
                            let data = tokio::fs::read(&local_path).await?;
                            let mut file = client.open(&remote_path, AfcFopenMode::Wr).await?;

                            // Clone sender and remote path for the callback
                            let sender = gui_sender_afc.clone();
                            let path_clone = remote_path.clone();

                            // Define the async callback
                            let callback = |((current, total), path): ((usize, usize), String)| {
                                let sender_clone = sender.clone();
                                async move {
                                    // Send progress update (ignore potential send error)
                                    let _ = sender_clone.send(GuiCommands::Afc(
                                        GuiAfcCommands::UploadProgress(path, current + 1, total),
                                    ));
                                }
                            };

                            // Use write_with_callback
                            file.write_with_callback(&data, callback, path_clone).await?;

                            Ok::<(), Box<dyn std::error::Error>>(())
                        }
                        .await;

                        let msg = match status {
                            Ok(_) => Ok(format!(
                                "Uploaded {} to {}",
                                local_path.file_name().unwrap().to_string_lossy(),
                                remote_path
                            )),
                            Err(e) => Err(e.to_string()),
                        };

                        // Send final status (success or error)
                        // This will implicitly clear the progress bar in the UI
                        gui_sender_afc
                            .send(GuiCommands::Afc(GuiAfcCommands::OperationStatus(msg)))
                            .unwrap(); // Or handle error
                    }
                }
                AfcCommands::CreateDirectory(path) => {
                    if let Some(client) = &mut afc_client {
                        let msg = match client.mk_dir(&path).await {
                            Ok(_) => Ok(format!("Created directory: {}", path)),
                            Err(e) => Err(e.to_string()),
                        };
                        gui_sender_afc
                            .send(GuiCommands::Afc(GuiAfcCommands::OperationStatus(msg)))
                            .unwrap();
                    }
                }
                AfcCommands::DeletePath(path) => {
                    if let Some(client) = &mut afc_client {
                        let msg = match client.remove_all(&path).await {
                            Ok(_) => Ok(format!("Deleted: {}", path)),
                            Err(e) => Err(e.to_string()),
                        };
                        gui_sender_afc
                            .send(GuiCommands::Afc(GuiAfcCommands::OperationStatus(msg)))
                            .unwrap();
                    }
                }
                AfcCommands::Disconnect => {
                    afc_client = None;
                }

                AfcCommands::DownloadDirectory(remote_dir_path, local_dir_path) => {
                    if let Some(client) = &mut afc_client {
                        let status_msg: Result<String, String>; // To report final status

                        // 1. Create the local directory
                        if let Err(e) = tokio::fs::create_dir_all(&local_dir_path).await {
                            status_msg = Err(format!(
                                "Failed to create local directory {}: {}",
                                local_dir_path.display(), e
                            ));
                        } else {
                            // 2. List remote directory contents
                            match client.list_dir(&remote_dir_path).await {
                                Ok(items) => {
                                    let mut item_count = 0;
                                    for item_name in items {
                                        if item_name == "." || item_name == ".." {
                                            continue;
                                        }
                                        item_count += 1;
                                        let child_remote_path = Path::new(&remote_dir_path)
                                            .join(&item_name)
                                            .to_string_lossy()
                                            .to_string();
                                        let child_local_path = local_dir_path.join(&item_name);

                                        // 3. Get info for each item
                                        match client.get_file_info(&child_remote_path).await {
                                            Ok(info) => {
                                                // 4. Send command to self for recursive processing
                                                if info.st_ifmt == "S_IFDIR" {
                                                    // It's a directory, send DownloadDirectory
                                                    if let Err(e) = self_afc_sender.send(
                                                        AfcCommands::DownloadDirectory(
                                                            child_remote_path,
                                                            child_local_path,
                                                        ),
                                                    ) {
                                                        error!("Failed to queue sub-directory download: {}", e);
                                                        // How to handle this? Maybe stop recursion?
                                                    }
                                                } else {
                                                    // It's a file, send DownloadFile
                                                     if let Err(e) = self_afc_sender.send(
                                                        AfcCommands::DownloadFile(
                                                            child_remote_path,
                                                            child_local_path,
                                                        ),
                                                    ) {
                                                        error!("Failed to queue file download: {}", e);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                error!(
                                                    "Failed to get info for {}: {:?}. Skipping.",
                                                    child_remote_path, e
                                                );
                                            }
                                        }
                                    }
                                    // Report success for *this level*
                                    status_msg = Ok(format!(
                                        "Queued {} items from {}",
                                        item_count, remote_dir_path
                                    ));
                                }
                                Err(e) => {
                                    status_msg = Err(format!(
                                        "Failed to list directory {}: {}",
                                        remote_dir_path, e
                                    ));
                                }
                            }
                        }

                        // Send status update for this specific directory operation
                        gui_sender_afc
                            .send(GuiCommands::Afc(GuiAfcCommands::OperationStatus(status_msg)))
                            .unwrap(); // Or handle error
                    } else {
                         // Should not happen if DownloadDirectory is only sent when connected
                         error!("Received DownloadDirectory command while not connected.");
                    }
                }

                AfcCommands::UploadDirectory(local_dir_path, remote_dir_path) => {
                    if let Some(client) = &mut afc_client {
                        let status_msg: Result<String, String>;

                        // 1. Create the remote directory
                        match client.mk_dir(&remote_dir_path).await {
                             Ok(_) => {
                                // 2. Read the local directory contents
                                match tokio::fs::read_dir(&local_dir_path).await {
                                    Ok(mut dir_entries) => {
                                        let mut item_count = 0;
                                        // Use loop and poll_next_entry for async iteration
                                        while let Ok(Some(entry)) = dir_entries.next_entry().await {
                                            item_count += 1;
                                            let entry_path = entry.path();
                                            let entry_name = entry.file_name();
                                            let child_remote_path = Path::new(&remote_dir_path)
                                                .join(&entry_name)
                                                .to_string_lossy()
                                                .to_string();

                                            // 3. Check if entry is file or directory
                                            match entry.file_type().await {
                                                Ok(file_type) => {
                                                    // 4. Queue up recursive command
                                                    if file_type.is_dir() {
                                                        if let Err(e) = self_afc_sender.send(
                                                            AfcCommands::UploadDirectory(
                                                                entry_path, // local subdir path
                                                                child_remote_path, // remote subdir path
                                                            ),
                                                        ) {
                                                             error!("Failed to queue sub-directory upload: {}", e);
                                                        }
                                                    } else if file_type.is_file() {
                                                         if let Err(e) = self_afc_sender.send(
                                                            AfcCommands::UploadFile(
                                                                entry_path, // local file path
                                                                child_remote_path, // remote file path
                                                            ),
                                                        ) {
                                                             error!("Failed to queue file upload: {}", e);
                                                        }
                                                    } else {
                                                         // Skip symlinks, sockets, etc.
                                                         log::warn!("Skipping unsupported file type at {:?}", entry_path);
                                                    }
                                                }
                                                Err(e) => {
                                                     error!("Failed to get file type for {:?}: {}. Skipping.", entry_path, e);
                                                }
                                            }
                                        }
                                        status_msg = Ok(format!(
                                            "Queued {} items from {}",
                                            item_count, local_dir_path.display()
                                        ));
                                    }
                                    Err(e) => {
                                         status_msg = Err(format!(
                                            "Failed to read local directory {}: {}",
                                            local_dir_path.display(), e
                                        ));
                                    }
                                }
                             }
                             Err(e) => {
                                // Handle cases where mkdir fails (e.g., directory exists, permissions)
                                // If it already exists, maybe proceed? Or report error?
                                // Let's report the error for now.
                                status_msg = Err(format!("Failed to create remote directory {}: {}", remote_dir_path, e));
                             }
                        }

                         // Send status update
                         gui_sender_afc
                            .send(GuiCommands::Afc(GuiAfcCommands::OperationStatus(status_msg)))
                            .unwrap(); // Or handle error
                    } else {
                         error!("Received UploadDirectory command while not connected.");
                    }
                }
            }
        }
    });

    let gui_sender_apps = gui_sender.clone();
    rt.spawn(async move {
        while let Some(command) = app_receiver.recv().await {
            match command {
                AppCommands::GetInstalledApps(dev) => {
                    let p = dev.to_provider(UsbmuxdAddr::default(), "afc_gui_apps");
                    let result = async {
                        let mut ic = InstallationProxyClient::connect(&p).await?;
                        let apps_plist = ic.get_apps(Some("User"), None).await?;
                        let mut apps_map = HashMap::new();
                        for (bundle_id, app_info) in apps_plist {
                            if let Some(name) = app_info
                                .as_dictionary()
                                .and_then(|dict| dict.get("CFBundleDisplayName"))
                                .and_then(|val| val.as_string())
                            {
                                apps_map.insert(name.to_string(), bundle_id);
                            } else {
                                // Fallback to bundle ID if display name is missing
                                if let Some(name) = app_info
                                    .as_dictionary()
                                    .and_then(|dict| dict.get("CFBundleIdentifier")) // CFBundleIdentifier often matches bundle_id
                                    .and_then(|val| val.as_string())
                                {
                                    apps_map.insert(name.to_string(), bundle_id);
                                } else {
                                    // Use bundle ID itself as name if nothing else works
                                    apps_map.insert(bundle_id.clone(), bundle_id);
                                }
                            }
                        }
                        Ok::<_, IdeviceError>(apps_map)
                    }
                    .await;

                    // Map IdeviceError to String for the GUI
                    let final_result = result.map_err(|e| e.to_string());

                    gui_sender_apps
                        .send(GuiCommands::InstalledApps(final_result))
                        .unwrap();
                }
            }
        }
    });

    let mut options = eframe::NativeOptions::default();
    // Smoother drag/resize on Windows/Linux
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        options.vsync = false;
        options.run_and_return = false;
        options.wgpu_options.present_mode = wgpu::PresentMode::AutoNoVsync;
    }

    // Prefer GL only on macOS Intel
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        options.renderer = eframe::Renderer::Glow;
    }

    #[cfg(target_os = "macos")]
    {
        options.viewport.icon = Some(std::sync::Arc::new(egui::IconData::default()));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let icon_bytes: &[u8] = include_bytes!("../icon.png");
        let d = eframe::icon_data::from_png_bytes(icon_bytes).expect("The icon data must be valid");
        options.viewport.icon = Some(std::sync::Arc::new(d));
    }

    eframe::run_native(
        &format!("AFC Finder v{}", env!("CARGO_PKG_VERSION")),
        options,
        Box::new(|c| {
            let ctx = c.egui_ctx.clone();
            rt.spawn(async move {
                loop {
                    ctx.request_repaint();
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
            });
            Ok(Box::new(app))
        }),
    )
    .unwrap();
}

// Commands sent from the GUI to the backend tasks
enum IdeviceCommands {
    GetDevices,
}

#[derive(Clone, PartialEq, Default)]
enum AfcMode {
    #[default]
    Root,
    Documents(String),
    Container(String),
    CrashReports,
}

enum AfcCommands {
    Connect(UsbmuxdDevice, AfcMode),
    Disconnect,
    ListDirectory(String),
    DownloadFile(String, PathBuf),
    UploadFile(PathBuf, String),
    CreateDirectory(String),
    DeletePath(String),
    DownloadDirectory(String, PathBuf),
    UploadDirectory(PathBuf, String),
}

enum AppCommands {
    GetInstalledApps(UsbmuxdDevice),
}

// Commands sent from backend tasks to the GUI
enum GuiCommands {
    NoUsbmuxd(IdeviceError),
    GetDevicesFailure(IdeviceError),
    Devices(HashMap<String, UsbmuxdDevice>),
    Afc(GuiAfcCommands),
    InstalledApps(Result<HashMap<String, String>, String>),
}

enum GuiAfcCommands {
    ConnectionStatus(Result<(), String>),
    DirectoryListing(Result<Vec<AfcItem>, String>),
    OperationStatus(Result<String, String>),
    UploadProgress(String, usize, usize),
    DownloadProgress(String, usize, usize),
}

// Main application state
struct MyApp {
    devices: Option<HashMap<String, UsbmuxdDevice>>,
    devices_placeholder: String,
    selected_device: String,
    afc_state: AfcState,
    gui_recv: UnboundedReceiver<GuiCommands>,
    idevice_sender: UnboundedSender<IdeviceCommands>,
    afc_sender: UnboundedSender<AfcCommands>,
    app_sender: UnboundedSender<AppCommands>,
    show_logs: bool,
}

#[derive(Default)]
struct AfcState {
    is_connected: bool,
    current_path: String,
    directory_listing: Option<Result<Vec<AfcItem>, String>>,
    status_message: String,
    selected_item: Option<String>,
    new_folder_name: String,
    show_new_folder_popup: bool,
    afc_mode: AfcMode,
    bundle_id_input: String,
    installed_apps: Option<Result<HashMap<String, String>, String>>,
    apps_loading: bool,
    upload_progress: Option<(String, f32)>,
    download_progress: Option<(String, f32)>,
    sort_column: SortColumn,
    sort_direction: SortDirection,
}

#[derive(Clone, Debug)]
pub struct AfcItem {
    pub name: String,
    pub info: FileInfo,
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f32 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f32 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f32 / (1024.0 * 1024.0 * 1024.0))
    }
}

// Add state for sorting
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum SortColumn {
    #[default]
    Name,
    Size,
    Modified,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

// Implement Default for SortColumn and SortDirection if needed, or initialize in AfcState::default()

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process messages from backend tasks
        while let Ok(msg) = self.gui_recv.try_recv() {
            ctx.request_repaint();
            match msg {
                GuiCommands::NoUsbmuxd(err) => {
                    self.devices_placeholder = format!("Failed to connect to usbmuxd: {:?}", err);
                }
                GuiCommands::GetDevicesFailure(err) => {
                    self.devices_placeholder = format!("Failed to get device list: {:?}", err);
                }
                GuiCommands::Devices(devs) => {
                    if self.selected_device.is_empty()
                        && devs.len() == 1
                        && let Some(name) = devs.keys().next()
                    {
                        self.selected_device = name.clone();
                        // Reset AFC state when auto-selecting
                        self.afc_state = AfcState::default();
                        self.afc_sender.send(AfcCommands::Disconnect).unwrap();
                    }

                    self.devices = Some(devs);
                }
                GuiCommands::InstalledApps(result) => {
                    self.afc_state.installed_apps = Some(result);
                    self.afc_state.apps_loading = false; // Mark as loaded (or failed)
                }
                GuiCommands::Afc(afc_msg) => match afc_msg {
                    GuiAfcCommands::ConnectionStatus(Ok(_)) => {
                        self.afc_state.is_connected = true;
                        self.afc_state.current_path = match self.afc_state.afc_mode {
                            AfcMode::Documents(_) => "/Documents".to_string(),
                            _ => "/".to_string(),
                        };
                        self.afc_sender
                            .send(AfcCommands::ListDirectory(
                                self.afc_state.current_path.clone(),
                            ))
                            .unwrap();
                    }
                    GuiAfcCommands::ConnectionStatus(Err(e)) => {
                        self.afc_state.status_message = format!("Connection failed: {}", e);
                    }
                    GuiAfcCommands::DirectoryListing(listing) => {
                        self.afc_state.directory_listing = Some(listing);
                        self.afc_state.selected_item = None; // Deselect on navigation
                        self.afc_state.status_message = "Connected".to_string();
                    }
                    GuiAfcCommands::UploadProgress(path, current, total) => {
                        if total > 0 {
                            let progress = current as f32 / total as f32;
                            self.afc_state.status_message = format!("Uploading {path}...");
                            self.afc_state.upload_progress = Some((path, progress));
                        }
                    }
                    GuiAfcCommands::DownloadProgress(path, bytes_read, total_bytes) => {
                        if total_bytes > 0 {
                            self.afc_state.status_message = format!("Downloading {path}...");
                            let progress = bytes_read as f32 / total_bytes as f32;
                            self.afc_state.download_progress = Some((path, progress));

                            // Ensure upload progress is cleared if a download starts
                            self.afc_state.upload_progress = None;
                        }
                    }
                    GuiAfcCommands::OperationStatus(result) => {
                        match result {
                            Ok(msg) => self.afc_state.status_message = msg,
                            Err(e) => self.afc_state.status_message = format!("Error: {}", e),
                        }
                        // Clear both upload and download progress
                        self.afc_state.upload_progress = None;
                        self.afc_state.download_progress = None;

                        // Refresh directory listing (existing logic)
                        self.afc_sender
                            .send(AfcCommands::ListDirectory(
                                self.afc_state.current_path.clone(),
                            ))
                            .unwrap();
                    }
                },
            }
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("AFC Finder");
                ui.separator();
                if let Some(devs) = &self.devices {
                    ComboBox::from_label("Device")
                        .selected_text(&self.selected_device)
                        .show_ui(ui, |ui| {
                            for name in devs.keys() {
                                if ui
                                    .selectable_value(&mut self.selected_device, name.clone(), name)
                                    .clicked()
                                {
                                    // Disconnect if we switch devices
                                    self.afc_state = AfcState::default();
                                    self.afc_sender.send(AfcCommands::Disconnect).unwrap();
                                }
                            }
                        });
                } else {
                    ui.label(&self.devices_placeholder);
                }

                if ui.button("Refresh Devices").clicked() {
                    self.selected_device = "".to_string();
                    self.afc_state = AfcState::default();
                    self.idevice_sender
                        .send(IdeviceCommands::GetDevices)
                        .unwrap();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.show_logs, "⛭ Logs");
                });
            });
        });

        if self.show_logs {
            egui::Window::new("Logs")
                .open(&mut self.show_logs)
                .show(ctx, |ui| {
                    egui_logger::logger_ui().show(ui);
                });
        }

        if !self.selected_device.is_empty() && !self.afc_state.is_connected {
            self.render_connection_ui(ctx);
        } else if self.afc_state.is_connected {
            self.render_explorer_ui(ctx);
        } else {
            self.render_default_ui(ctx);
        }

        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            if let Some((_path, progress)) = &self.afc_state.upload_progress {
                ui.horizontal(|ui| {
                    ui.label(&self.afc_state.status_message); // Show status (e.g., Uploading...)
                    ui.add(egui::ProgressBar::new(*progress).show_percentage());
                });
            } else if let Some((_path, progress)) = &self.afc_state.download_progress {
                ui.horizontal(|ui| {
                    ui.label(&self.afc_state.status_message); // Show status (e.g., Downloading...)
                    ui.add(egui::ProgressBar::new(*progress).show_percentage());
                });
            } else {
                ui.label(&self.afc_state.status_message); // Show normal status message
            }
        });
    }
}

impl MyApp {
    fn render_default_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Select a device to continue");
        });
    }

    fn render_connection_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Connect to AFC");
            ui.separator();

            let requires_bundle_id = !matches!(self.afc_state.afc_mode, AfcMode::Root,)
                && !matches!(self.afc_state.afc_mode, AfcMode::CrashReports);

            // Trigger app fetch if needed and not already loading/loaded
            if requires_bundle_id
                && self.afc_state.installed_apps.is_none()
                && !self.afc_state.apps_loading
                && let Some(dev) = self
                    .devices
                    .as_ref()
                    .and_then(|d| d.get(&self.selected_device))
            {
                self.app_sender
                    .send(AppCommands::GetInstalledApps(dev.clone()))
                    .unwrap();
                self.afc_state.apps_loading = true; // Set loading flag
                self.afc_state.status_message = "Loading installed apps...".to_string();
            }

            ui.radio_value(&mut self.afc_state.afc_mode, AfcMode::Root, "AFC Jail");
            ui.radio_value(
                &mut self.afc_state.afc_mode,
                AfcMode::Documents("".into()), // Use dummy value, actual ID is in bundle_id_input
                "Application Documents",
            );
            ui.radio_value(
                &mut self.afc_state.afc_mode,
                AfcMode::Container("".into()), // Use dummy value
                "Application Container",
            );
            ui.radio_value(
                &mut self.afc_state.afc_mode,
                AfcMode::CrashReports,
                "Crash Repots",
            );

            if requires_bundle_id {
                ui.separator();
                ui.label("Application:");

                // --- Searchable Bundle ID Input ---
                ui.add(
                    TextEdit::singleline(&mut self.afc_state.bundle_id_input)
                        .hint_text("Type App Name or Bundle ID..."),
                );

                match &self.afc_state.installed_apps {
                    Some(Ok(apps)) => {
                        let filter = self.afc_state.bundle_id_input.to_lowercase();
                        let filtered_apps: Vec<_> = apps
                            .iter()
                            .filter(|(name, id)| {
                                filter.is_empty() || // Show all if filter is empty
                                name.to_lowercase().contains(&filter) ||
                                id.to_lowercase().contains(&filter)
                            })
                            .collect();

                        ui.label(format!("{} apps found:", filtered_apps.len())); // Show count

                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .show(ui, |ui| {
                                if filtered_apps.is_empty() && !filter.is_empty() {
                                    ui.label("(No matching apps found)");
                                } else {
                                    for (name, id) in filtered_apps {
                                        let label = format!("{} ({})", name, id);
                                        // Use selectable_value to visually indicate selection and set the input field
                                        if ui
                                            .selectable_value(
                                                &mut self.afc_state.bundle_id_input,
                                                id.clone(),
                                                label,
                                            )
                                            .clicked()
                                        {}
                                    }
                                }
                            });
                    }
                    Some(Err(e)) => {
                        ui.colored_label(Color32::RED, format!("Error loading apps: {}", e));
                    }
                    None => {
                        if self.afc_state.apps_loading {
                            ui.spinner();
                            ui.label("Loading apps...");
                        } else {
                            // This case might happen briefly before the request is sent
                            ui.label("...");
                        }
                    }
                }
                // --- End Searchable Input ---
                ui.separator();
            }

            // Make sure bundle ID is not empty if required before allowing connection
            let can_connect = !requires_bundle_id || !self.afc_state.bundle_id_input.is_empty();

            if ui
                .add_enabled(can_connect, egui::Button::new("Connect"))
                .clicked()
                && let Some(dev) = self
                    .devices
                    .as_ref()
                    .and_then(|d| d.get(&self.selected_device))
            {
                // Use the final value from bundle_id_input
                let mode = match self.afc_state.afc_mode.clone() {
                    AfcMode::Root => AfcMode::Root,
                    AfcMode::Documents(_) => {
                        AfcMode::Documents(self.afc_state.bundle_id_input.clone())
                    }
                    AfcMode::Container(_) => {
                        AfcMode::Container(self.afc_state.bundle_id_input.clone())
                    }
                    AfcMode::CrashReports => AfcMode::CrashReports,
                };
                self.afc_sender
                    .send(AfcCommands::Connect(dev.clone(), mode))
                    .unwrap();
                self.afc_state.status_message = "Connecting...".to_string();
            }
        });
    }

    fn render_explorer_ui(&mut self, ctx: &egui::Context) {
        // --- Popups ---
        if self.afc_state.show_new_folder_popup {
            egui::Window::new("Create New Folder")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.text_edit_singleline(&mut self.afc_state.new_folder_name);
                    ui.horizontal(|ui| {
                        if ui.button("Create").clicked() {
                            let new_path = Path::new(&self.afc_state.current_path)
                                .join(&self.afc_state.new_folder_name)
                                .to_string_lossy()
                                .to_string();
                            self.afc_sender
                                .send(AfcCommands::CreateDirectory(new_path))
                                .unwrap();
                            self.afc_state.show_new_folder_popup = false;
                            self.afc_state.new_folder_name.clear();
                        }
                        if ui.button("Cancel").clicked() {
                            self.afc_state.show_new_folder_popup = false;
                            self.afc_state.new_folder_name.clear();
                        }
                    });
                });
        }

        // --- Main UI ---
        egui::CentralPanel::default().show(ctx, |ui| {
            // Toolbar
            ui.horizontal(|ui| {
                // Determine jail root and if we are at it
                let jail_root = match self.afc_state.afc_mode {
                    AfcMode::Documents(_) => "/Documents",
                    _ => "/",
                };
                let at_jail_root = self.afc_state.current_path == jail_root;

                // Disable "Up" button if at jail root
                if ui
                    .add_enabled(!at_jail_root, egui::Button::new("⬆ Up"))
                    .clicked()
                    && let Some(parent) = Path::new(&self.afc_state.current_path).parent()
                {
                    self.afc_state.status_message = "Loading directory...".to_string();
                    let parent_str = parent.to_string_lossy().to_string();

                    // Ensure we don't navigate "up" past the jail root
                    self.afc_state.current_path = if parent_str.len() < jail_root.len() {
                        jail_root.to_string()
                    } else {
                        parent_str
                    };

                    self.afc_sender
                        .send(AfcCommands::ListDirectory(
                            self.afc_state.current_path.clone(),
                        ))
                        .unwrap();
                }

                // --- Editable path bar ---
                let mut trigger_nav = false;

                // Manually lay out the TextEdit and "Go" button to fill space
                let go_button_width = 35.0; // Width for "Go" button
                let text_edit_width =
                    ui.available_width() - go_button_width - ui.spacing().item_spacing.x;

                let response = ui.add_sized(
                    [text_edit_width, ui.available_height()],
                    egui::TextEdit::singleline(&mut self.afc_state.current_path)
                        .font(egui::TextStyle::Monospace),
                );

                // Trigger navigation on "Enter"
                if response.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                    trigger_nav = true;
                }

                // Trigger navigation on "Go" button click
                if ui.button("Go").clicked() {
                    trigger_nav = true;
                }

                if trigger_nav {
                    self.afc_state.status_message = "Loading directory...".to_string();
                    self.afc_sender
                        .send(AfcCommands::ListDirectory(
                            self.afc_state.current_path.clone(),
                        ))
                        .unwrap();
                }
            });
            ui.separator();

            // Action Buttons
            ui.horizontal(|ui| {
                if ui.button("Upload File...").clicked()
                    && let Some(path) = FileDialog::new().pick_file()
                {
                    let remote_path = Path::new(&self.afc_state.current_path)
                        .join(path.file_name().unwrap())
                        .to_string_lossy()
                        .to_string();
                    self.afc_sender
                        .send(AfcCommands::UploadFile(path, remote_path))
                        .unwrap();
                }

                if ui.button("Upload Folder...").clicked()
                    && let Some(local_folder_path) = FileDialog::new()
                        .set_title("Select Folder to Upload")
                        .pick_folder()
                {
                    if let Some(folder_name) = local_folder_path.file_name() {
                        let remote_target_dir = Path::new(&self.afc_state.current_path)
                            .join(folder_name) // Target directory will have the same name
                            .to_string_lossy()
                            .to_string();

                        self.afc_state.status_message = format!(
                            "Starting upload of folder {}...",
                            folder_name.to_string_lossy()
                        );
                        self.afc_sender
                            .send(AfcCommands::UploadDirectory(
                                local_folder_path, // Source local dir
                                remote_target_dir, // Target remote dir path
                            ))
                            .unwrap();
                    } else {
                        self.afc_state.status_message =
                            "Error: Could not get folder name.".to_string();
                        error!(
                            "Failed to get file_name from picked folder: {:?}",
                            local_folder_path
                        );
                    }
                }

                if ui.button("New Folder...").clicked() {
                    self.afc_state.show_new_folder_popup = true;
                }
                ui.add_enabled(
                    self.afc_state.selected_item.is_some(),
                    egui::Button::new("Download"),
                )
                .on_hover_text("Select a file to download")
                .clicked()
                .then(|| {
                    if let Some(selected_name) = &self.afc_state.selected_item {
                        // Find the corresponding AfcItem to check its type
                        let selected_item_info = self
                            .afc_state
                            .directory_listing
                            .as_ref()
                            .and_then(|res| res.as_ref().ok()) // &Result<...> -> Option<&Vec<...>>
                            .and_then(|items| items.iter().find(|i| &i.name == selected_name)); // Option<&AfcItem>

                        if let Some(item) = selected_item_info {
                            let is_dir = item.info.st_ifmt == "S_IFDIR";
                            let remote_path = Path::new(&self.afc_state.current_path)
                                .join(selected_name)
                                .to_string_lossy()
                                .to_string();

                            if is_dir {
                                // Prompt for local save *directory*
                                if let Some(local_save_dir) = FileDialog::new()
                                    .set_directory("/") // Start at root or user's home?
                                    .set_title("Select Download Location for Folder")
                                    // Use file_name of the remote dir as suggestion
                                    .set_file_name(&item.name)
                                    .pick_folder()
                                // Use pick_folder
                                {
                                    // The user selected a directory. We might want to create the
                                    // target folder *inside* this selected directory.
                                    let final_local_path = local_save_dir.join(&item.name);

                                    self.afc_state.status_message =
                                        format!("Starting download of folder {}...", item.name);
                                    self.afc_sender
                                        .send(AfcCommands::DownloadDirectory(
                                            remote_path,
                                            final_local_path, // Path where the folder itself will be created
                                        ))
                                        .unwrap();
                                }
                            } else {
                                // Prompt for local save *file* (existing logic)
                                if let Some(local_save_path) = FileDialog::new()
                                    .set_file_name(selected_name) // Use file_name directly
                                    .save_file()
                                {
                                    self.afc_state.status_message =
                                        format!("Starting download of file {}...", item.name);
                                    self.afc_sender
                                        .send(AfcCommands::DownloadFile(
                                            remote_path,
                                            local_save_path,
                                        ))
                                        .unwrap();
                                }
                            }
                        } else {
                            // This case should ideally not happen if an item is selected
                            self.afc_state.status_message =
                                "Error: Could not find selected item info.".to_string();
                            error!(
                                "Selected item '{}' not found in directory listing state.",
                                selected_name
                            );
                        }
                    }
                });

                ui.add_enabled(
                    self.afc_state.selected_item.is_some(),
                    egui::Button::new("Delete"),
                )
                .on_hover_text("Select an item to delete")
                .clicked()
                .then(|| {
                    if let Some(selected) = &self.afc_state.selected_item {
                        let path_to_delete = Path::new(&self.afc_state.current_path)
                            .join(selected)
                            .to_string_lossy()
                            .to_string();
                        self.afc_sender
                            .send(AfcCommands::DeletePath(path_to_delete))
                            .unwrap();
                    }
                });
            });
            ui.separator();

            egui::Grid::new("directory_header")
                .num_columns(3)
                .striped(false) // No stripes for header
                .show(ui, |ui| {
                    // Clickable Header: Name
                    if ui.button("Name").clicked() {
                        if self.afc_state.sort_column == SortColumn::Name {
                            self.afc_state.sort_direction = match self.afc_state.sort_direction {
                                SortDirection::Ascending => SortDirection::Descending,
                                SortDirection::Descending => SortDirection::Ascending,
                            };
                        } else {
                            self.afc_state.sort_column = SortColumn::Name;
                            self.afc_state.sort_direction = SortDirection::Ascending;
                        }
                    }
                    // Clickable Header: Size
                    if ui.button("Size").clicked() {
                        if self.afc_state.sort_column == SortColumn::Size {
                            self.afc_state.sort_direction = match self.afc_state.sort_direction {
                                SortDirection::Ascending => SortDirection::Descending,
                                SortDirection::Descending => SortDirection::Ascending,
                            };
                        } else {
                            self.afc_state.sort_column = SortColumn::Size;
                            self.afc_state.sort_direction = SortDirection::Ascending;
                        }
                    }
                    // Clickable Header: Date Modified
                    if ui.button("Date Modified").clicked() {
                        if self.afc_state.sort_column == SortColumn::Modified {
                            self.afc_state.sort_direction = match self.afc_state.sort_direction {
                                SortDirection::Ascending => SortDirection::Descending,
                                SortDirection::Descending => SortDirection::Ascending,
                            };
                        } else {
                            self.afc_state.sort_column = SortColumn::Modified;
                            self.afc_state.sort_direction = SortDirection::Ascending;
                        }
                    }
                    ui.end_row();
                });

            // Directory Listing
            egui::ScrollArea::vertical().show(ui, |ui| {
                match &mut self.afc_state.directory_listing {
                    // Need mutable access to sort
                    Some(Ok(items)) => {
                        // --- Apply Sorting ---
                        let sort_col = self.afc_state.sort_column;
                        let sort_dir = self.afc_state.sort_direction;
                        items.sort_by(|a, b| {
                            let ordering = match sort_col {
                                SortColumn::Name => {
                                    a.name.to_lowercase().cmp(&b.name.to_lowercase())
                                }
                                SortColumn::Size => a.info.size.cmp(&b.info.size),
                                SortColumn::Modified => a.info.modified.cmp(&b.info.modified),
                            };
                            match sort_dir {
                                SortDirection::Ascending => ordering,
                                SortDirection::Descending => ordering.reverse(),
                            }
                        });
                        // --- End Sorting ---

                        // Display using Grid
                        egui::Grid::new("directory_listing_grid")
                            .num_columns(3)
                            .striped(true) // Enable stripes for items
                            .show(ui, |ui| {
                                for afc_item in items {
                                    let item_name = &afc_item.name;
                                    let is_dir = afc_item.info.st_ifmt == "S_IFDIR";
                                    let icon = if is_dir { "📁" } else { "📄" };
                                    let label_text = format!("{} {}", icon, item_name);

                                    // Create a selectable label for the name column
                                    let name_label = egui::Button::selectable(
                                        self.afc_state.selected_item.as_deref() == Some(item_name),
                                        label_text,
                                    );
                                    let response = ui.add(name_label);

                                    // Handle clicks on the name label
                                    if response.clicked() {
                                        self.afc_state.selected_item = Some(item_name.clone());
                                    }
                                    if response.double_clicked() && is_dir {
                                        self.afc_state.status_message =
                                            "Loading directory...".to_string();
                                        self.afc_state.current_path =
                                            Path::new(&self.afc_state.current_path)
                                                .join(item_name)
                                                .to_string_lossy()
                                                .to_string();
                                        self.afc_sender
                                            .send(AfcCommands::ListDirectory(
                                                self.afc_state.current_path.clone(),
                                            ))
                                            .unwrap();
                                    }

                                    // Size Column (show "--" for directories)
                                    let size_str = if is_dir {
                                        "--".to_string()
                                    } else {
                                        format_size(afc_item.info.size)
                                    };
                                    ui.label(size_str);

                                    // Modified Date Column
                                    let modified_str =
                                        afc_item.info.modified.format("%Y-%m-%d %H:%M").to_string();
                                    ui.label(modified_str);

                                    ui.end_row();
                                }
                            });
                    }
                    Some(Err(e)) => {
                        ui.colored_label(Color32::RED, e);
                    }
                    None => {
                        ui.label("Loading directory...");
                    }
                }
            });
        });
    }
}
