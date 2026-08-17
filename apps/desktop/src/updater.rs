use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/richtunic/mi-rectificacion-mx/releases/latest";
const RELEASE_DOWNLOAD_PATH: &str = "/richtunic/mi-rectificacion-mx/releases/download/";
const MAX_INSTALLER_BYTES: u64 = 250 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub title: String,
    pub notes: String,
    pub release_url: String,
    pub asset: UpdateAsset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAsset {
    pub name: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

pub fn check_for_update(current_version: &str) -> Result<Option<AvailableUpdate>, String> {
    let release = github_client()?
        .get(LATEST_RELEASE_API)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("No fue posible consultar las actualizaciones: {error}"))?
        .json::<GithubRelease>()
        .map_err(|error| {
            format!("GitHub devolvió una respuesta de actualización inválida: {error}")
        })?;
    available_update_from_release(current_version, release)
}

fn available_update_from_release(
    current_version: &str,
    release: GithubRelease,
) -> Result<Option<AvailableUpdate>, String> {
    let current = parse_version(current_version)?;
    let latest = parse_version(&release.tag_name)?;
    if latest <= current {
        return Ok(None);
    }

    let asset = select_platform_asset(&release.assets).ok_or_else(|| {
        "La nueva versión no incluye un instalador compatible con este equipo".to_owned()
    })?;
    validate_asset(asset)?;
    Ok(Some(AvailableUpdate {
        version: latest.to_string(),
        title: release
            .name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("Mi Rectificación MX {latest}")),
        notes: release
            .body
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "Esta versión incluye mejoras y correcciones.".to_owned()),
        release_url: release.html_url,
        asset: UpdateAsset {
            name: asset.name.clone(),
            download_url: asset.browser_download_url.clone(),
            sha256: asset
                .digest
                .as_deref()
                .and_then(|value| value.strip_prefix("sha256:"))
                .map(str::to_ascii_lowercase)
                .ok_or_else(|| "La Release no incluye una huella SHA-256 verificable".to_owned())?,
            size: asset.size,
        },
    }))
}

fn parse_version(value: &str) -> Result<Version, String> {
    Version::parse(value.trim().trim_start_matches('v'))
        .map_err(|_| format!("La versión publicada `{value}` no tiene un formato válido"))
}

fn select_platform_asset(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    assets
        .iter()
        .find(|asset| asset_matches_platform(&asset.name))
}

fn asset_matches_platform(name: &str) -> bool {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return name.ends_with("_aarch64.dmg");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return name.ends_with("_x86_64.dmg");
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return name.ends_with("_x64.msi");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return name.ends_with("_x86_64.AppImage");
    #[allow(unreachable_code)]
    false
}

fn validate_asset(asset: &GithubAsset) -> Result<(), String> {
    if asset.size == 0 || asset.size > MAX_INSTALLER_BYTES {
        return Err("El instalador publicado tiene un tamaño inesperado".to_owned());
    }
    validate_download_url(&asset.browser_download_url)?;
    if Path::new(&asset.name)
        .file_name()
        .and_then(|value| value.to_str())
        != Some(asset.name.as_str())
    {
        return Err("El nombre del instalador publicado no es seguro".to_owned());
    }
    let Some(digest) = asset.digest.as_deref() else {
        return Err("La Release no incluye una huella SHA-256 verificable".to_owned());
    };
    let Some(value) = digest.strip_prefix("sha256:") else {
        return Err("GitHub publicó una huella de integridad no compatible".to_owned());
    };
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err("La huella SHA-256 publicada no es válida".to_owned());
    }
    Ok(())
}

fn validate_download_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| "La dirección del instalador no es válida".to_owned())?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.path().starts_with(RELEASE_DOWNLOAD_PATH)
    {
        return Err("El instalador no proviene de la Release oficial del proyecto".to_owned());
    }
    Ok(url)
}

pub fn download_and_launch(update: &AvailableUpdate) -> Result<PathBuf, String> {
    let destination = download_update(update)?;
    launch_installer(&destination)?;
    Ok(destination)
}

fn download_update(update: &AvailableUpdate) -> Result<PathBuf, String> {
    let url = validate_download_url(&update.asset.download_url)?;
    if update.asset.size == 0 || update.asset.size > MAX_INSTALLER_BYTES {
        return Err("El instalador supera el tamaño permitido".to_owned());
    }

    let directory = std::env::temp_dir()
        .join("mi-rectificacion-mx-updates")
        .join(&update.version);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("No fue posible preparar la descarga: {error}"))?;
    let destination = directory.join(&update.asset.name);
    let temporary = directory.join(format!("{}.download", update.asset.name));
    let mut response = github_client()?
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| format!("No fue posible descargar la actualización: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_INSTALLER_BYTES)
    {
        return Err("La descarga supera el tamaño permitido".to_owned());
    }

    let mut file = File::create(&temporary)
        .map_err(|error| format!("No fue posible guardar la actualización: {error}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("La descarga se interrumpió: {error}"))?;
        if read == 0 {
            break;
        }
        downloaded += read as u64;
        if downloaded > MAX_INSTALLER_BYTES {
            let _ = fs::remove_file(&temporary);
            return Err("La descarga supera el tamaño permitido".to_owned());
        }
        file.write_all(&buffer[..read])
            .map_err(|error| format!("No fue posible guardar la actualización: {error}"))?;
        hasher.update(&buffer[..read]);
    }
    file.sync_all()
        .map_err(|error| format!("No fue posible finalizar la descarga: {error}"))?;
    if downloaded != update.asset.size {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "La descarga está incompleta: se esperaban {} bytes y se recibieron {downloaded}",
            update.asset.size
        ));
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != update.asset.sha256 {
        let _ = fs::remove_file(&temporary);
        return Err("La actualización descargada no coincide con la huella publicada".to_owned());
    }
    if destination.exists() {
        fs::remove_file(&destination)
            .map_err(|error| format!("No fue posible reemplazar la descarga anterior: {error}"))?;
    }
    fs::rename(&temporary, &destination)
        .map_err(|error| format!("No fue posible finalizar la actualización: {error}"))?;
    Ok(destination)
}

fn github_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(format!("Mi-Rectificacion-MX/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("No fue posible preparar la conexión segura: {error}"))
}

fn launch_installer(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        open::that(path).map_err(|error| format!("No fue posible abrir el instalador: {error}"))?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("msiexec")
            .arg("/i")
            .arg(path)
            .spawn()
            .map_err(|error| format!("No fue posible iniciar el instalador de Windows: {error}"))?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("No fue posible preparar AppImage: {error}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("No fue posible preparar AppImage: {error}"))?;
        std::process::Command::new(path)
            .spawn()
            .map_err(|error| format!("No fue posible iniciar la nueva AppImage: {error}"))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("La actualización integrada no está disponible en esta plataforma".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: Vec<GithubAsset>) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_owned(),
            name: Some(format!("Versión {tag}")),
            body: Some("- Mejora uno\n- Corrección dos".to_owned()),
            html_url: format!(
                "https://github.com/richtunic/mi-rectificacion-mx/releases/tag/{tag}"
            ),
            assets,
        }
    }

    fn asset(name: &str) -> GithubAsset {
        GithubAsset {
            name: name.to_owned(),
            browser_download_url: format!(
                "https://github.com/richtunic/mi-rectificacion-mx/releases/download/v0.2.0/{name}"
            ),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
            size: 12_000_000,
        }
    }

    #[test]
    fn semantic_versions_handle_beta_and_stable_releases() {
        assert!(parse_version("v0.2.0").unwrap() > parse_version("0.2.0-beta.3").unwrap());
        assert!(parse_version("v1.0.0").unwrap() > parse_version("0.9.9").unwrap());
    }

    #[test]
    fn ignores_a_release_that_is_not_newer() {
        let current =
            available_update_from_release("0.2.0", release("v0.2.0", Vec::new())).unwrap();
        assert_eq!(current, None);
    }

    #[test]
    fn selects_only_the_installer_for_the_current_platform() {
        let assets = vec![
            asset("Mi.Rectificacion.MX_0.2.0_aarch64.dmg"),
            asset("Mi.Rectificacion.MX_0.2.0_x86_64.dmg"),
            asset("Mi.Rectificacion.MX_0.2.0_x64.msi"),
            asset("Mi.Rectificacion.MX_0.2.0_x86_64.AppImage"),
        ];
        let update = available_update_from_release("0.1.0", release("v0.2.0", assets))
            .unwrap()
            .unwrap();
        assert!(asset_matches_platform(&update.asset.name));
        assert_eq!(update.asset.sha256.len(), 64);
    }

    #[test]
    fn rejects_downloads_outside_the_official_release_path() {
        let unsafe_asset = GithubAsset {
            name: "update.dmg".to_owned(),
            browser_download_url: "https://example.com/update.dmg".to_owned(),
            digest: Some(format!("sha256:{}", "b".repeat(64))),
            size: 10,
        };
        assert!(validate_asset(&unsafe_asset).is_err());
    }
}
