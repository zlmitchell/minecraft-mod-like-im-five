<p align="center">
  <img src="src-tauri/icons/icon.png" alt="Minecraft Mods, Easy" width="180" />
</p>

# Minecraft Mods, Easy

One-click Minecraft mod manager designed for kids. Click a profile, get a working
modded Minecraft with shaders, performance mods, and an HD resource pack — all
visible in the standard Microsoft / Mojang launcher when you're done.

No mod-loader install steps, no JVM-args spreadsheet, no "click Apply on the
shader pack." It does the boring parts.

## Credit where it's due

This tool is a thin shim over [**Modrinth**](https://modrinth.com) — the
open mod hosting platform that catalogs every mod, shader, and resource pack
this app installs. Their public API does the heavy lifting: version
resolution, loader filtering, sha1 hash lookup. None of this would exist
without them.

The mods themselves are made by the Minecraft modding community — Modrinth
links every project to its author. Some you'll see installed out of the box:

- [Sodium](https://modrinth.com/mod/sodium), [Lithium](https://modrinth.com/mod/lithium) — CaffeineMC
- [Iris](https://modrinth.com/mod/iris) — IrisShaders team
- [Fabric API](https://modrinth.com/mod/fabric-api) — FabricMC
- [Complementary Reimagined](https://modrinth.com/shader/complementary-reimagined) — EminGT
- [Faithful 32x](https://modrinth.com/resourcepack/faithful-32x) — Faithful Team
- [LambDynamicLights](https://modrinth.com/mod/lambdynamiclights) — LambdAurora
- [Continuity](https://modrinth.com/mod/continuity) — PepperCode1

Loader installs use [FabricMC's meta API](https://meta.fabricmc.net) directly.
"Latest" Minecraft version comes from [Mojang's piston-meta manifest](https://launchermeta.mojang.com).

If you build on top of this, give Modrinth credit too.

## What's in each profile

| Profile | MC | Loader | What you get |
|---|---|---|---|
| Latest & Beautiful | 1.21.9 (pinned for shader-pack compat) | Fabric | Sodium + Iris + Lithium + Complementary Reimagined (auto-enabled) + LambDynamicLights + Continuity + Faithful 32x |
| Bleeding Edge Performance | latest (auto-resolved each install) | Fabric | Sodium suite + Lithium + FerriteCore + Entity Culling + ImmediatelyFast + ModernFix + Iris (no shader pack pre-selected) + JEI + Jade + Xaero's Minimap + AppleSkin + Continuity + LambDynamicLights + Faithful 32x |
| Adventure with Cobblemon & Create | 1.21.1 | NeoForge | Cobblemon + Create + JEI + Jade + Xaero's Minimap + AppleSkin + GeckoLib (uses the Minecraft launcher's bundled Java to run the NeoForge installer) |

## Using it

1. Download the latest `.exe` from the [Releases](../../releases) page.
2. Run it. The standard Minecraft launcher needs to have been opened at
   least once first (so `.minecraft/` exists).
3. Click a profile. The status badge shows the detected `.minecraft` path —
   click it if you need to point at a non-default install.
4. The app will close any running launcher / Minecraft instance (with your
   confirmation), install everything, then re-open the launcher.
5. Pick the new profile from the launcher's dropdown and Play.

Dragging a `.jar` onto the window will identify the mod via Modrinth's hash
lookup and tell you whether it's compatible with your active profile.

## Building from source

Cross-compiles a Windows `.exe` from a Linux container — works on any host
with Docker:

```bash
docker build -f Dockerfile.build-exe -o dist .
```

Or, on Windows host:

```pwsh
pwsh ./scripts/build-exe.ps1
```

Output: `dist/minecraft-mod-like-im-five.exe`. First run takes ~10 min
(downloads MSVC SDK + compiles Tauri); subsequent runs ~2 min from cache
mounts.

For local dev (Rust + Node toolchain on host):

```pwsh
pwsh ./scripts/gen-icons.ps1
npm install
npm run dev
```

### Testing profile edits without a `data-latest` release

By default the app pulls `profiles.yaml` from the `data-latest` GitHub
release on every launch — which means edits to your local YAML are
invisible to a built `.exe`. Set `MMLE5_LOCAL_DATA` to the directory
containing your YAML to bypass GitHub and read straight from disk:

```pwsh
$env:MMLE5_LOCAL_DATA = "$(Resolve-Path .\data)"
.\dist\minecraft-mod-like-im-five.exe
```

The version line in the header will read `Data local-dev: <path>` while
this is on. Unset the variable (or open a new shell) to return to the
production cached + GitHub flow.

## Adding a profile or mod

Edit `data/profiles.yaml`, rebuild. Mod entries are Modrinth project slugs
(the last segment of the project's URL: `modrinth.com/mod/<slug>`).

```yaml
- id: my-profile
  name: My Profile
  short_description: ...
  minecraft_version: "1.21.9"   # or `latest` to auto-resolve
  loader: fabric
  mods:
    - { source: modrinth, slug: fabric-api,  role: required }
    - { source: modrinth, slug: sodium,      role: performance }
  shaders:
    - { source: modrinth, slug: complementary-reimagined, default: true }
  resource_packs:
    - { source: modrinth, slug: faithful-32x, default: true }
```

The `default: true` flag on a shader or resource pack means the app
pre-enables it (writing to `iris.properties` or `options.txt`).

## How "manage itself" works

- **Renovate** opens PRs for outdated dependencies. Minor + patch updates
  auto-merge once CI passes; major Tauri / loader updates need human review.
- **release-please** watches `main` for [conventional commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, etc.) and opens release PRs that bump the version and
  generate `CHANGELOG.md`. Merging that PR cuts a tag + GitHub Release.
- **build.yml** runs on every PR — required to be green before merge.
- **release.yml** builds the Windows `.exe` when a release is cut and
  attaches it to the GitHub Release.

For Renovate, install the [Renovate GitHub App](https://github.com/apps/renovate)
on the repo (zero config). Or, run the included `renovate.yml` workflow with
a `RENOVATE_TOKEN` repo secret.

## License

TBD — pick one before tagging 1.0.
