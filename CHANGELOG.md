# Changelog

## [1.0.1](https://github.com/zlmitchell/minecraft-mod-like-im-five/compare/v1.0.0...v1.0.1) (2026-05-07)


### Bug Fixes

* **icons:** derive Windows ICO from real icon.png artwork ([9c5afbb](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/9c5afbbde808aece385b90a773d3685d2fb4f4f7))

## [1.0.0](https://github.com/zlmitchell/minecraft-mod-like-im-five/compare/v0.4.0...v1.0.0) (2026-05-07)


### ⚠ BREAKING CHANGES

* **profiles:** profile id "latest-beautiful" no longer exists. Anyone with that profile installed via the launcher will keep working (.minecraft entry is unaffected), but the profile won't appear in the app's card list.

### Features

* NeoForge, modpack profiles, mod cache, server-pack export ([d93d786](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/d93d786d10e0e9ffaee3d5d50dd0cc478f14c41b))
* **profiles:** remove "Latest & Beautiful" profile ([8f375b0](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/8f375b0a1ab82301bf934f7459a9a0058dda68ff))

## [0.4.0](https://github.com/zlmitchell/minecraft-mod-like-im-five/compare/v0.3.0...v0.4.0) (2026-05-07)


### Features

* major install + UX overhaul ([7e41adb](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/7e41adb5036dac9a1a8e40192e9b351559610379))
* publish data via rolling release; show app + data versions ([80a1fac](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/80a1fac1b9133c5e28c96fbc142af3fa18be3baa))

## [0.3.0](https://github.com/zlmitchell/minecraft-mod-like-im-five/compare/v0.2.1...v0.3.0) (2026-05-06)


### Features

* **release:** add portable Windows exe alongside the installer ([e226aa0](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/e226aa07d997d19c558e4b064026a999f516faa8))
* **updater:** auto-check for new versions on launch ([b68dca0](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/b68dca0ecf6288acffccccbe38bc400f708a3665))

## [0.2.1](https://github.com/zlmitchell/minecraft-mod-like-im-five/compare/v0.2.0...v0.2.1) (2026-05-06)


### Bug Fixes

* more thorough Minecraft launcher detection ([17ae81d](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/17ae81d42f960d06c1e500f6285da4d414f8ae67))

## [0.2.0](https://github.com/zlmitchell/minecraft-mod-like-im-five/compare/v0.1.1...v0.2.0) (2026-05-06)


### Features

* **ci:** build macOS, Linux, and Windows bundles on release ([3b8996d](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/3b8996ddb8fd3263527a932942ef2928f899436a))
* **signing:** set up Tauri self-update signing ([17fbc9a](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/17fbc9a2262d88bd8848a93f0a1619f255481d53))


### Bug Fixes

* **ci:** make rebuild.yml's icon step conditional ([6b99e3d](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/6b99e3dd47651766d699f4942929748da2d1f310))
* don't detect ourselves as a running Minecraft process ([17fbc9a](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/17fbc9a2262d88bd8848a93f0a1619f255481d53))

## [0.1.1](https://github.com/zlmitchell/minecraft-mod-like-im-five/compare/v0.1.0...v0.1.1) (2026-05-06)


### Bug Fixes

* **release-please:** point package path at src-tauri/ where Cargo.toml lives ([85fe887](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/85fe887fb57c342261ffee8c1b3a0682052afd4e))
* **release-please:** track repo root with extra-files updaters ([e709dab](https://github.com/zlmitchell/minecraft-mod-like-im-five/commit/e709dabddb1c2694a21bdad2fe45d9aa8f9a1676))
