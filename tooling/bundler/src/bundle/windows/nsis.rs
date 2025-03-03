// Copyright 2019-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

#[cfg(target_os = "windows")]
use crate::bundle::windows::util::try_sign;
use crate::{
  bundle::{
    common::CommandExt,
    windows::util::{
      download, download_and_verify, extract_zip, HashAlgorithm, NSIS_OUTPUT_FOLDER_NAME,
      NSIS_UPDATER_OUTPUT_FOLDER_NAME, WEBVIEW2_BOOTSTRAPPER_URL, WEBVIEW2_X64_INSTALLER_GUID,
      WEBVIEW2_X86_INSTALLER_GUID,
    },
  },
  Settings,
};
use tauri_utils::display_path;

use anyhow::Context;
use handlebars::{to_json, Handlebars};
use log::{info, warn};
use tauri_utils::{
  config::{NSISInstallerMode, WebviewInstallMode},
  resources::resource_relpath,
};

use std::{
  collections::{BTreeMap, HashMap},
  fs::{copy, create_dir_all, remove_dir_all, rename, write},
  path::{Path, PathBuf},
  process::Command,
};

// URLS for the NSIS toolchain.
#[cfg(target_os = "windows")]
const NSIS_URL: &str =
  "https://github.com/tauri-apps/binary-releases/releases/download/nsis-3/nsis-3.zip";
#[cfg(target_os = "windows")]
const NSIS_SHA1: &str = "057e83c7d82462ec394af76c87d06733605543d4";
const NSIS_APPLICATIONID_URL: &str = "https://github.com/tauri-apps/binary-releases/releases/download/nsis-plugins-v0/NSIS-ApplicationID.zip";
const NSIS_TAURI_UTILS: &str =
  "https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.1.1/nsis_tauri_utils.dll";
const NSIS_TAURI_UTILS_SHA1: &str = "A21C67CF5AB6D4274AFFF0D68CFCE680D213DDC7";

#[cfg(target_os = "windows")]
const NSIS_REQUIRED_FILES: &[&str] = &[
  "makensis.exe",
  "Bin/makensis.exe",
  "Stubs/lzma-x86-unicode",
  "Stubs/lzma_solid-x86-unicode",
  "Plugins/x86-unicode/ApplicationID.dll",
  "Plugins/x86-unicode/nsis_tauri_utils.dll",
  "Include/MUI2.nsh",
  "Include/FileFunc.nsh",
  "Include/x64.nsh",
  "Include/nsDialogs.nsh",
  "Include/WinMessages.nsh",
];
#[cfg(not(target_os = "windows"))]
const NSIS_REQUIRED_FILES: &[&str] = &[
  "Plugins/x86-unicode/ApplicationID.dll",
  "Plugins/x86-unicode/nsis_tauri_utils.dll",
];

/// Runs all of the commands to build the NSIS installer.
/// Returns a vector of PathBuf that shows where the NSIS installer was created.
pub fn bundle_project(settings: &Settings, updater: bool) -> crate::Result<Vec<PathBuf>> {
  let tauri_tools_path = dirs_next::cache_dir().unwrap().join("tauri");
  let nsis_toolset_path = tauri_tools_path.join("NSIS");

  if !nsis_toolset_path.exists() {
    get_and_extract_nsis(&nsis_toolset_path, &tauri_tools_path)?;
  } else if NSIS_REQUIRED_FILES
    .iter()
    .any(|p| !nsis_toolset_path.join(p).exists())
  {
    warn!("NSIS directory is missing some files. Recreating it.");
    std::fs::remove_dir_all(&nsis_toolset_path)?;
    get_and_extract_nsis(&nsis_toolset_path, &tauri_tools_path)?;
  }

  build_nsis_app_installer(settings, &nsis_toolset_path, &tauri_tools_path, updater)
}

// Gets NSIS and verifies the download via Sha1
fn get_and_extract_nsis(nsis_toolset_path: &Path, _tauri_tools_path: &Path) -> crate::Result<()> {
  info!("Verifying NSIS package");

  #[cfg(target_os = "windows")]
  {
    let data = download_and_verify(NSIS_URL, NSIS_SHA1, HashAlgorithm::Sha1)?;
    info!("extracting NSIS");
    extract_zip(&data, _tauri_tools_path)?;
    rename(_tauri_tools_path.join("nsis-3.08"), nsis_toolset_path)?;
  }

  let nsis_plugins = nsis_toolset_path.join("Plugins");

  let data = download(NSIS_APPLICATIONID_URL)?;
  info!("extracting NSIS ApplicationID plugin");
  extract_zip(&data, &nsis_plugins)?;

  create_dir_all(nsis_plugins.join("x86-unicode"))?;

  copy(
    nsis_plugins
      .join("ReleaseUnicode")
      .join("ApplicationID.dll"),
    nsis_plugins.join("x86-unicode").join("ApplicationID.dll"),
  )?;

  let data = download_and_verify(NSIS_TAURI_UTILS, NSIS_TAURI_UTILS_SHA1, HashAlgorithm::Sha1)?;
  write(
    nsis_plugins
      .join("x86-unicode")
      .join("nsis_tauri_utils.dll"),
    data,
  )?;

  Ok(())
}

fn build_nsis_app_installer(
  settings: &Settings,
  _nsis_toolset_path: &Path,
  tauri_tools_path: &Path,
  updater: bool,
) -> crate::Result<Vec<PathBuf>> {
  let arch = match settings.binary_arch() {
    "x86_64" => "x64",
    "x86" => "x86",
    "aarch64" => "arm64",
    target => {
      return Err(crate::Error::ArchError(format!(
        "unsupported target: {}",
        target
      )))
    }
  };

  info!("Target: {}", arch);

  #[cfg(target_os = "windows")]
  {
    let main_binary = settings
      .binaries()
      .iter()
      .find(|bin| bin.main())
      .ok_or_else(|| anyhow::anyhow!("Failed to get main binary"))?;
    let app_exe_source = settings.binary_path(main_binary);
    try_sign(&app_exe_source, settings)?;
  }

  #[cfg(not(target_os = "windows"))]
  info!("Code signing is currently only supported on Windows hosts, skipping...");

  let output_path = settings.project_out_directory().join("nsis").join(arch);
  if output_path.exists() {
    remove_dir_all(&output_path)?;
  }
  create_dir_all(&output_path)?;

  let mut data = BTreeMap::new();

  let bundle_id = settings.bundle_identifier();
  let manufacturer = bundle_id.split('.').nth(1).unwrap_or(bundle_id);

  #[cfg(not(target_os = "windows"))]
  {
    let mut dir = dirs_next::cache_dir().unwrap();
    dir.extend(["tauri", "NSIS", "Plugins", "x86-unicode"]);
    data.insert("additional_plugins_path", to_json(dir));
  }

  data.insert("arch", to_json(arch));
  data.insert("bundle_id", to_json(bundle_id));
  data.insert("manufacturer", to_json(manufacturer));
  data.insert("product_name", to_json(settings.product_name()));
  data.insert("copyright", to_json(settings.copyright_string()));

  let version = settings.version_string();
  data.insert("version", to_json(version));

  data.insert(
    "allow_downgrades",
    to_json(settings.windows().allow_downgrades),
  );

  let mut install_mode = NSISInstallerMode::CurrentUser;
  let mut languages = vec!["English".into()];
  let mut custom_template_path = None;
  let mut custom_language_files = None;
  if let Some(nsis) = &settings.windows().nsis {
    custom_template_path = nsis.template.clone();
    custom_language_files = nsis.custom_language_files.clone();
    install_mode = nsis.install_mode;
    if let Some(langs) = &nsis.languages {
      languages.clear();
      languages.extend_from_slice(langs);
    }
    if let Some(license) = &nsis.license {
      data.insert("license", to_json(dunce::canonicalize(license)?));
    }
    if let Some(installer_icon) = &nsis.installer_icon {
      data.insert(
        "installer_icon",
        to_json(dunce::canonicalize(installer_icon)?),
      );
    }
    if let Some(header_image) = &nsis.header_image {
      data.insert("header_image", to_json(dunce::canonicalize(header_image)?));
    }
    if let Some(sidebar_image) = &nsis.sidebar_image {
      data.insert(
        "sidebar_image",
        to_json(dunce::canonicalize(sidebar_image)?),
      );
    }

    data.insert(
      "display_language_selector",
      to_json(nsis.display_language_selector && languages.len() > 1),
    );
  }
  data.insert(
    "install_mode",
    to_json(match install_mode {
      NSISInstallerMode::CurrentUser => "currentUser",
      NSISInstallerMode::PerMachine => "perMachine",
      NSISInstallerMode::Both => "both",
    }),
  );

  let languages_data =  languages
  .iter()
  .filter_map(|lang| {
    if let Some(data) = get_lang_data(lang, custom_language_files.as_ref()) {
      Some(data)
    } else {
      log::warn!("Custom tauri messages for {lang} are not translated.\nIf it is a valid language listed on <https://github.com/kichik/nsis/tree/9465c08046f00ccb6eda985abbdbf52c275c6c4d/Contrib/Language%20files>, please open a Tauri feature request\n or you can provide a custom language file for it in `tauri.conf.json > tauri > bundle > windows > nsis > custom_language_files`");
      None
    }
  }).collect::<Vec<_>>();

  data.insert("languages", to_json(languages.clone()));
  data.insert(
    "language_files",
    to_json(
      languages_data
        .iter()
        .map(|d| d.0.clone())
        .collect::<Vec<_>>(),
    ),
  );

  let main_binary = settings
    .binaries()
    .iter()
    .find(|bin| bin.main())
    .ok_or_else(|| anyhow::anyhow!("Failed to get main binary"))?;
  data.insert(
    "main_binary_name",
    to_json(main_binary.name().replace(".exe", "")),
  );
  data.insert(
    "main_binary_path",
    to_json(settings.binary_path(main_binary).with_extension("exe")),
  );

  let out_file = "nsis-output.exe";
  data.insert("out_file", to_json(out_file));

  let resources = generate_resource_data(settings)?;
  data.insert("resources", to_json(resources));

  let binaries = generate_binaries_data(settings)?;
  data.insert("binaries", to_json(binaries));

  let silent_webview2_install = if let WebviewInstallMode::DownloadBootstrapper { silent }
  | WebviewInstallMode::EmbedBootstrapper { silent }
  | WebviewInstallMode::OfflineInstaller { silent } =
    settings.windows().webview_install_mode
  {
    silent
  } else {
    true
  };

  let webview2_install_mode = if updater {
    WebviewInstallMode::DownloadBootstrapper {
      silent: silent_webview2_install,
    }
  } else {
    let mut webview_install_mode = settings.windows().webview_install_mode.clone();
    if let Some(fixed_runtime_path) = settings.windows().webview_fixed_runtime_path.clone() {
      webview_install_mode = WebviewInstallMode::FixedRuntime {
        path: fixed_runtime_path,
      };
    } else if let Some(wix) = &settings.windows().wix {
      if wix.skip_webview_install {
        webview_install_mode = WebviewInstallMode::Skip;
      }
    }
    webview_install_mode
  };

  let webview2_installer_args = to_json(if silent_webview2_install {
    "/silent"
  } else {
    ""
  });

  data.insert("webview2_installer_args", to_json(webview2_installer_args));
  data.insert(
    "install_webview2_mode",
    to_json(match webview2_install_mode {
      WebviewInstallMode::DownloadBootstrapper { silent: _ } => "downloadBootstrapper",
      WebviewInstallMode::EmbedBootstrapper { silent: _ } => "embedBootstrapper",
      WebviewInstallMode::OfflineInstaller { silent: _ } => "offlineInstaller",
      _ => "",
    }),
  );

  match webview2_install_mode {
    WebviewInstallMode::EmbedBootstrapper { silent: _ } => {
      let webview2_bootstrapper_path = tauri_tools_path.join("MicrosoftEdgeWebview2Setup.exe");
      std::fs::write(
        &webview2_bootstrapper_path,
        download(WEBVIEW2_BOOTSTRAPPER_URL)?,
      )?;
      data.insert(
        "webview2_bootstrapper_path",
        to_json(webview2_bootstrapper_path),
      );
    }
    WebviewInstallMode::OfflineInstaller { silent: _ } => {
      let guid = if arch == "x64" {
        WEBVIEW2_X64_INSTALLER_GUID
      } else {
        WEBVIEW2_X86_INSTALLER_GUID
      };
      let offline_installer_path = tauri_tools_path
        .join("Webview2OfflineInstaller")
        .join(guid)
        .join(arch);
      create_dir_all(&offline_installer_path)?;
      let webview2_installer_path =
        offline_installer_path.join("MicrosoftEdgeWebView2RuntimeInstaller.exe");
      if !webview2_installer_path.exists() {
        std::fs::write(
          &webview2_installer_path,
          download(
            &format!("https://msedge.sf.dl.delivery.mp.microsoft.com/filestreamingservice/files/{}/MicrosoftEdgeWebView2RuntimeInstaller{}.exe",
              guid,
              arch.to_uppercase(),
            ),
          )?,
        )?;
      }
      data.insert("webview2_installer_path", to_json(webview2_installer_path));
    }
    _ => {}
  }

  let mut handlebars = Handlebars::new();
  handlebars.register_escape_fn(|s| {
    let mut output = String::new();
    for c in s.chars() {
      match c {
        '\"' => output.push_str("$\\\""),
        '$' => output.push_str("$$"),
        '`' => output.push_str("$\\`"),
        '\n' => output.push_str("$\\n"),
        '\t' => output.push_str("$\\t"),
        '\r' => output.push_str("$\\r"),
        _ => output.push(c),
      }
    }
    output
  });
  if let Some(path) = custom_template_path {
    handlebars
      .register_template_string("installer.nsi", std::fs::read_to_string(path)?)
      .map_err(|e| e.to_string())
      .expect("Failed to setup custom handlebar template");
  } else {
    handlebars
      .register_template_string("installer.nsi", include_str!("./templates/installer.nsi"))
      .map_err(|e| e.to_string())
      .expect("Failed to setup handlebar template");
  }
  let installer_nsi_path = output_path.join("installer.nsi");
  write(
    &installer_nsi_path,
    encoding_rs::UTF_8
      .encode(handlebars.render("installer.nsi", &data)?.as_str())
      .0,
  )?;

  for (lang, data) in languages_data.iter() {
    if let Some((content, encoding)) = data {
      write(
        output_path.join(lang).with_extension("nsh"),
        encoding.encode(content).0,
      )?;
    }
  }

  let package_base_name = format!(
    "{}_{}_{}-setup",
    main_binary.name().replace(".exe", ""),
    settings.version_string(),
    arch,
  );

  let nsis_output_path = output_path.join(out_file);
  let nsis_installer_path = settings.project_out_directory().to_path_buf().join(format!(
    "bundle/{}/{}.exe",
    if updater {
      NSIS_UPDATER_OUTPUT_FOLDER_NAME
    } else {
      NSIS_OUTPUT_FOLDER_NAME
    },
    package_base_name
  ));
  create_dir_all(nsis_installer_path.parent().unwrap())?;

  info!(action = "Running"; "makensis.exe to produce {}", display_path(&nsis_installer_path));

  #[cfg(target_os = "windows")]
  let mut nsis_cmd = Command::new(_nsis_toolset_path.join("makensis.exe"));
  #[cfg(not(target_os = "windows"))]
  let mut nsis_cmd = Command::new("makensis");

  nsis_cmd
    .arg(match settings.log_level() {
      log::Level::Error => "-V1",
      log::Level::Warn => "-V2",
      log::Level::Info => "-V3",
      _ => "-V4",
    })
    .arg(installer_nsi_path)
    .current_dir(output_path)
    .piped()
    .context("error running makensis.exe")?;

  rename(nsis_output_path, &nsis_installer_path)?;

  // Code signing is currently only supported on Windows hosts
  #[cfg(target_os = "windows")]
  try_sign(&nsis_installer_path, settings)?;

  Ok(vec![nsis_installer_path])
}

/// BTreeMap<OriginalPath, (ParentOfTargetPath, TargetPath)>
type ResourcesMap = BTreeMap<PathBuf, (String, PathBuf)>;
fn generate_resource_data(settings: &Settings) -> crate::Result<ResourcesMap> {
  let mut resources = ResourcesMap::new();
  let cwd = std::env::current_dir()?;

  let mut added_resources = Vec::new();

  for src in settings.resource_files() {
    let src = src?;
    let resource_path = dunce::canonicalize(cwd.join(&src))?;

    // In some glob resource paths like `assets/**/*` a file might appear twice
    // because the `tauri_utils::resources::ResourcePaths` iterator also reads a directory
    // when it finds one. So we must check it before processing the file.
    if added_resources.contains(&resource_path) {
      continue;
    }
    added_resources.push(resource_path.clone());

    let target_path = resource_relpath(&src);
    resources.insert(
      resource_path,
      (
        target_path
          .parent()
          .map(|p| p.to_string_lossy().to_string())
          .unwrap_or_default(),
        target_path,
      ),
    );
  }

  Ok(resources)
}

/// BTreeMap<OriginalPath, TargetFileName>
type BinariesMap = BTreeMap<PathBuf, String>;
fn generate_binaries_data(settings: &Settings) -> crate::Result<BinariesMap> {
  let mut binaries = BinariesMap::new();
  let cwd = std::env::current_dir()?;

  for src in settings.external_binaries() {
    let src = src?;
    let binary_path = dunce::canonicalize(cwd.join(&src))?;
    let dest_filename = src
      .file_name()
      .expect("failed to extract external binary filename")
      .to_string_lossy()
      .replace(&format!("-{}", settings.target()), "");
    binaries.insert(binary_path, dest_filename);
  }

  for bin in settings.binaries() {
    if !bin.main() {
      let bin_path = settings.binary_path(bin);
      binaries.insert(
        bin_path.clone(),
        bin_path
          .file_name()
          .expect("failed to extract external binary filename")
          .to_string_lossy()
          .to_string(),
      );
    }
  }

  Ok(binaries)
}

fn get_lang_data(
  lang: &str,
  custom_lang_files: Option<&HashMap<String, PathBuf>>,
) -> Option<(
  String,
  Option<(&'static str, &'static encoding_rs::Encoding)>,
)> {
  use encoding_rs::*;

  if let Some(path) = custom_lang_files.and_then(|h| h.get(lang)) {
    return Some((
      dunce::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .to_string(),
      None,
    ));
  }

  let lang_file = format!("{lang}.nsh");
  match lang.to_lowercase().as_str() {
    "arabic" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/Arabic.nsh"),
        UTF_16LE,
      )),
    )),
    "dutch" => Some((
      lang_file,
      Some((include_str!("./templates/nsis-languages/Dutch.nsh"), UTF_8)),
    )),
    "english" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/English.nsh"),
        UTF_8,
      )),
    )),
    "japanese" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/Japanese.nsh"),
        UTF_8,
      )),
    )),
    "korean" => Some((
      lang_file,
      Some((include_str!("./templates/nsis-languages/Korean.nsh"), UTF_8)),
    )),
    "portuguesebr" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/PortugueseBR.nsh"),
        UTF_8,
      )),
    )),
    "tradchinese" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/TradChinese.nsh"),
        UTF_8,
      )),
    )),
    "simpchinese" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/SimpChinese.nsh"),
        UTF_8,
      )),
    )),
    "french" => Some((
      lang_file,
      Some((include_str!("./templates/nsis-languages/French.nsh"), UTF_8)),
    )),
    "spanish" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/Spanish.nsh"),
        UTF_8,
      )),
    )),
    "spanishinternational" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/SpanishInternational.nsh"),
        UTF_8,
      )),
    )),
    "persian" => Some((
      lang_file,
      Some((
        include_str!("./templates/nsis-languages/Persian.nsh"),
        UTF_16LE,
      )),
    )),
    _ => None,
  }
}
