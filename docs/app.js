const repository = 'richtunic/mi-rectificacion-mx';
const releaseApi = `https://api.github.com/repos/${repository}/releases/latest`;
const platforms = {
  'mac-arm': '_aarch64.dmg',
  'mac-intel': '_x86_64.dmg',
  windows: '_x64.msi',
  linux: '_x86_64.AppImage',
};

document.getElementById('year').textContent = new Date().getFullYear();

function preferredPlatform() {
  const agent = navigator.userAgent;
  if (/Windows/i.test(agent)) return 'windows';
  if (/Linux/i.test(agent) && !/Android/i.test(agent)) return 'linux';
  return null;
}

async function detectMacArchitecture() {
  if (!/Macintosh|Mac OS X/i.test(navigator.userAgent)) return null;
  if (!navigator.userAgentData?.getHighEntropyValues) return null;
  try {
    const { architecture } = await navigator.userAgentData.getHighEntropyValues(['architecture']);
    if (architecture === 'arm') return 'mac-arm';
    if (architecture === 'x86') return 'mac-intel';
  } catch (_) {
    // Si el navegador no expone la arquitectura, se muestran ambas opciones.
  }
  return null;
}

function recommend(platform) {
  if (!platform) return;
  const card = document.querySelector(`[data-platform="${platform}"]`);
  if (!card) return;
  card.classList.add('recommended');
  const mainLink = document.getElementById('primary-download');
  mainLink.href = card.querySelector('.download-link').href;
  mainLink.textContent = `Descargar para ${platform.startsWith('mac') ? 'macOS' : platform === 'windows' ? 'Windows' : 'Linux'} ↗`;
}

function validAsset(asset, tag, suffix) {
  if (!asset?.name?.endsWith(suffix) || !asset?.size || !/^sha256:[a-f0-9]{64}$/i.test(asset.digest || '')) return false;
  try {
    const url = new URL(asset.browser_download_url);
    return url.protocol === 'https:' && url.hostname === 'github.com' &&
      url.pathname.startsWith(`/${repository}/releases/download/${encodeURIComponent(tag)}/`);
  } catch (_) {
    return false;
  }
}

async function refreshRelease() {
  try {
    const response = await fetch(releaseApi, { headers: { Accept: 'application/vnd.github+json' } });
    if (!response.ok) return;
    const release = await response.json();
    const tag = release.tag_name;
    if (!/^v?\d+\.\d+\.\d+(?:-[\w.]+)?$/.test(tag)) return;
    const assets = Object.fromEntries(Object.entries(platforms).map(([platform, suffix]) => [
      platform,
      release.assets?.find((asset) => validAsset(asset, tag, suffix)),
    ]));
    if (Object.values(assets).some((asset) => !asset)) return;

    const version = tag.replace(/^v/, '');
    document.getElementById('hero-version').textContent = `Versión ${version}`;
    document.getElementById('download-version').textContent = `Versión ${version}`;
    for (const [platform, asset] of Object.entries(assets)) {
      const card = document.querySelector(`[data-platform="${platform}"]`);
      card.querySelector('.download-link').href = asset.browser_download_url;
      card.querySelector('.asset-meta').firstChild.textContent = `${(asset.size / 1_000_000).toFixed(1)} MB · `;
      card.querySelector('.asset-version').textContent = version;
      card.querySelector('.asset-digest').textContent = asset.digest.slice(7);
    }
    const recommended = document.querySelector('.download-card.recommended');
    if (recommended) document.getElementById('primary-download').href = recommended.querySelector('.download-link').href;
  } catch (_) {
    // Los enlaces verificados de la versión incluida en el sitio siguen disponibles.
  }
}

recommend(preferredPlatform());
detectMacArchitecture().then(recommend);
refreshRelease();
