// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;
use std::path::PathBuf;

use crate::logger;

/// One entry of a `*-rbxPkgManifest.txt` block (v0 format, 4 lines per package).
#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub signature: String,
    pub packed_size: u64,
    #[allow(dead_code)]
    pub size: u64,
}

/// Parses the manifest; stops at `RobloxPlayerLauncher.exe` (the stock launcher, not used here).
pub fn parse_manifest(data: &str) -> Result<Vec<Package>, String> {
    let mut lines = data.lines().map(str::trim).filter(|l| !l.is_empty());

    let header = lines.next().unwrap_or("");
    if header != "v0" {
        return Err(format!("unexpected package manifest version: {header} (expected v0)"));
    }

    let mut packages = Vec::new();
    while let (Some(name), Some(signature), Some(packed), Some(unpacked)) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    {
        if name == "RobloxPlayerLauncher.exe" {
            break;
        }
        let (Ok(packed_size), Ok(size)) = (packed.parse::<u64>(), unpacked.parse::<u64>()) else {
            break;
        };
        packages.push(Package {
            name: name.to_string(),
            signature: signature.to_string(),
            packed_size,
            size,
        });
    }
    Ok(packages)
}

/// Directory each package extracts into, relative to the version directory.
/// Defaults from Froststrap's remote config, hardcoded to stay dependency- and
/// request-free (Roblox has never shipped an override for these).
pub fn package_map(is_studio: bool) -> HashMap<&'static str, &'static str> {
    let mut map = HashMap::from([
        ("Libraries.zip", ""),
        ("redist.zip", ""),
        ("shaders.zip", "shaders"),
        ("ssl.zip", "ssl"),
        ("WebView2.zip", ""),
        ("WebView2RuntimeInstaller.zip", "WebView2RuntimeInstaller"),
        ("content-avatar.zip", "content/avatar"),
        ("content-configs.zip", "content/configs"),
        ("content-fonts.zip", "content/fonts"),
        ("content-sky.zip", "content/sky"),
        ("content-sounds.zip", "content/sounds"),
        ("content-textures2.zip", "content/textures"),
        ("content-models.zip", "content/models"),
        ("content-textures3.zip", "PlatformContent/pc/textures"),
        ("content-terrain.zip", "PlatformContent/pc/terrain"),
        ("content-platform-fonts.zip", "PlatformContent/pc/fonts"),
        (
            "content-platform-dictionaries.zip",
            "PlatformContent/pc/shared_compression_dictionaries",
        ),
        ("extracontent-luapackages.zip", "ExtraContent/LuaPackages"),
        ("extracontent-translations.zip", "ExtraContent/translations"),
        ("extracontent-models.zip", "ExtraContent/models"),
        ("extracontent-textures.zip", "ExtraContent/textures"),
        ("extracontent-places.zip", "ExtraContent/places"),
    ]);
    if is_studio {
        map.extend([
            ("RobloxStudio.zip", ""),
            ("LibrariesQt5.zip", ""),
            ("content-studio_svg_textures.zip", "content/studio_svg_textures"),
            ("content-qt_translations.zip", "content/qt_translations"),
            ("content-api-docs.zip", "content/api_docs"),
            ("extracontent-scripts.zip", "ExtraContent/scripts"),
            ("studiocontent-models.zip", "StudioContent/models"),
            ("studiocontent-textures.zip", "StudioContent/textures"),
            ("BuiltInPlugins.zip", "BuiltInPlugins"),
            ("BuiltInStandalonePlugins.zip", "BuiltInStandalonePlugins"),
            ("ApplicationConfig.zip", "ApplicationConfig"),
            ("Plugins.zip", "Plugins"),
            ("Qml.zip", "Qml"),
            ("StudioFonts.zip", "StudioFonts"),
            ("RibbonConfig.zip", "RibbonConfig"),
        ]);
    } else {
        map.extend([("RobloxApp.zip", "")]);
    }
    map
}

/// Defuses path traversal and absolute paths in zip entries; returns None to skip.
pub fn sanitize_zip_path(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let mut out = PathBuf::new();
    for component in PathBuf::from(&name).components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    if name.ends_with('/') || out.as_os_str().is_empty() {
        return None;
    }
    Some(out)
}

/// Extracts `archive` into `target_dir`, overwriting existing files.
pub fn extract_zip(archive_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| format!("{}: {e}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.to_string();
        let Some(relative) = sanitize_zip_path(&name) else {
            continue;
        };
        let out_path = target_dir.join(&relative);
        if let Some(parent) = out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Disk contents win: the version directory is always treated as disposable.
        let _ = std::fs::remove_file(&out_path);
        let mut out_file = std::fs::File::create(&out_path)
            .map_err(|e| format!("create {}: {e}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out_file)
            .map_err(|e| format!("{}: {e}", relative.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                let _ = std::fs::set_permissions(
                    &out_path,
                    std::fs::Permissions::from_mode(mode),
                );
            }
        }
    }
    Ok(())
}

/// Recursively copies `src` over `dst`, creating directories as needed (used to
/// lay user modifications on top of a fresh version directory).
pub fn copy_dir_overwrite(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let entry_path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_overwrite(&entry_path, &dest_path)?;
        } else {
            std::fs::copy(&entry_path, &dest_path)?;
        }
    }
    Ok(())
}

pub fn remove_dir_forced(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_dir_all(path) {
        if path.exists() {
            logger::error(&format!("Failed to delete {}: {e}", path.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "v0\n\
        RobloxApp.zip\nA1B2C3\n100\n200\n\
        shaders.zip\nD4E5F6\n10\n20\n\
        RobloxPlayerLauncher.exe\nFFEE\n1\n1\nextracontent-x.zip\nAA\n1\n2\n";

    #[test]
    fn manifest_parse_v0() {
        let packages = parse_manifest(SAMPLE).unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "RobloxApp.zip");
        assert_eq!(packages[0].signature, "A1B2C3");
        assert_eq!(packages[0].packed_size, 100);
        assert_eq!(packages[1].name, "shaders.zip");
    }

    #[test]
    fn manifest_rejects_unknown_version() {
        assert!(parse_manifest("v1\nx\ny\n1\n2\n").is_err());
    }

    #[test]
    fn package_map_overrides() {
        let player = package_map(false);
        assert_eq!(player["RobloxApp.zip"], "");
        assert_eq!(player["shaders.zip"], "shaders");
        let studio = package_map(true);
        assert!(studio.contains_key("RobloxStudio.zip"));
        assert!(!studio.contains_key("RobloxApp.zip"));
    }

    #[test]
    fn zip_path_sanitized() {
        assert_eq!(
            sanitize_zip_path("content/Textures/Icon.png").unwrap(),
            PathBuf::from("content/Textures/Icon.png")
        );
        assert_eq!(
            sanitize_zip_path("a\\b.png").unwrap(),
            PathBuf::from("a/b.png")
        );
        assert!(sanitize_zip_path("../evil.png").is_none());
        assert!(sanitize_zip_path("/abs.png").is_none());
        assert!(sanitize_zip_path("folder/").is_none());
    }
}
