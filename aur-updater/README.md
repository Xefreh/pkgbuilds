# AUR auto-updater

Orchestrateur quotidien qui détecte les nouvelles versions upstream de chaque
paquet AUR, met à jour `PKGBUILD` + `.SRCINFO`, pousse sur l'AUR, et envoie un
récapitulatif Telegram (mis à jour / déjà à jour / upstream cassé / échec).

## Stack
- Python (stdlib) — orchestrateur et parsing PKGBUILD
- Scripts bash par paquet pour la détection de version (ex: `fetch-version.sh`)
- systemd timer — déclenchement quotidien à 20:00
- TOML — manifeste de configuration

## Layout

```
pkgbuilds/
├── aur-updater/
│   ├── updater.py            # orchestrateur (CLI: --dry-run, --only <pkg>)
│   ├── pkgbuild.py           # lecture/édition pkgver, pkgrel
│   ├── notify.py             # notification Telegram (digest unique)
│   ├── config.toml           # manifeste des paquets
│   ├── aur-updater.service   # unit oneshot (User, EnvironmentFile)
│   ├── aur-updater.timer      # OnCalendar 20:00
│   ├── aur-updater.env.example # template secrets Telegram
│   └── README.md
└── <pkg>/                    # chaque paquet (submodule)
    ├── PKGBUILD
    ├── .SRCINFO
    └── fetch-version.sh      # script de détection (sort la version sur stdout)
```

## Contrat du script de version (`fetch-version.sh`)

- Sortir **sur stdout uniquement la version** (ex: `3.1.8`).
- `exit 0` si OK ; `exit` non-zero si l'upstream est cassé (CDN modifié, etc.).
- L'utilisateur est libre d'écrire le probing comme il veut (curl, gallop
  search, parsing de page, API JSON…).

La version détectée est validée par `version_regex` (config) ; une sortie non
conforme marque le paquet `BROKEN` (⚠️) — c'est la garde contre un CDN qui
renverrait autre chose qu'une version.

## Installation

### 1. Secrets Telegram

```sh
cp aur-updater/aur-updater.env.example ~/.config/aur-updater.env
$EDITOR ~/.config/aur-updater.env   # remplir TG_BOT_TOKEN et TG_CHAT_ID
chmod 600 ~/.config/aur-updater.env
```

Récupérer le chat id : envoyer un message au bot puis
`curl "https://api.telegram.org/bot<TOKEN>/getUpdates"`.

### 2. SSH pour l'AUR

Clé SSH configurée pour `aur@aur.archlinux.org` (voir le wiki AUR). Le remote
SSH de chaque paquet est `aur_remote` dans `config.toml`.

### 3. Dépendances Arch

```sh
pacman -S pacman-contrib   # fournit updpkgsums
```

### 4. Installer le timer systemd (--user)

```sh
mkdir -p ~/.config/systemd/user
ln -sf "$PWD/aur-updater/aur-updater.service" ~/.config/systemd/user/
ln -sf "$PWD/aur-updater/aur-updater.timer"   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now aur-updater.timer
```

Ajuster `WorkingDirectory` / `ExecStart` / `EnvironmentFile` dans le `.service`
si le dépôt n'est pas à `~/work/github.com/Xefreh/pkgbuilds`.

## Utilisation

```sh
# Test à blanc : détecte et affiche, ne pousse rien.
python aur-updater/updater.py --dry-run

# Ne traiter qu'un paquet.
python aur-updater/updater.py --only zcode-appimage

# Exécution réelle (commit + push AUR + Telegram).
python aur-updater/updater.py

# Logs systemd.
journalctl --user -u aur-updater.service
```

## États notifiés (digest Telegram unique)

| État        | Icône | Sens                                             |
|-------------|-------|--------------------------------------------------|
| UPDATED     | ✅    | pkgver bumpé, checksums régénérés, poussé AUR    |
| UP_TO_DATE  | ⏸    | version détectée == pkgver courant               |
| BROKEN      | ⚠️    | script de version en échec ou sortie non regex   |
| FAILED      | ❌    | échec makepkg/updpkgsums/git push (rollback auto) |
| WARN        | 🟡    | downgrade détecté (nouvelle version < courante)   |

## Limite connue

Si un CDN renvoie `200` pour n'importe quelle version (faux positif), la
garde regex ne le détecte pas. L'étape `updpkgsums` télécharge réellement
l'asset ; un fichier absent/corrompu fait échouer l'étape (→ `FAILED`), ce qui
protège en partie contre ce cas. Une sanity check optionnelle de l'asset
(taille/magic) est prévue comme extension future.
