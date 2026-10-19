# Changelog

## [0.4.1](https://github.com/EvilNick2/ron-mod-manager/compare/v0.4.0...v0.4.1) (2026-10-19)


### Bug Fixes

* **ui:** render release notes as markdown in the update modal ([6b40586](https://github.com/EvilNick2/ron-mod-manager/commit/6b405865d2500930f4ab6798db79af56fa1b9015))

## [0.4.0](https://github.com/EvilNick2/ron-mod-manager/compare/v0.3.0...v0.4.0) (2026-10-19)


### Features

* **drive:** add a Drive v3 client and an in-memory fake for tests ([b3c1260](https://github.com/EvilNick2/ron-mod-manager/commit/b3c1260d538586927ca17bc2b3b0065fd0b93ec1))
* **google:** sign in with Google using a loopback redirect and PKCE ([bc332e8](https://github.com/EvilNick2/ron-mod-manager/commit/bc332e88544c522eb09a4f19b404c11232127192))
* **grid:** group installed mods into a section per modpack ([13f4095](https://github.com/EvilNick2/ron-mod-manager/commit/13f4095934798732e4dad28de05afd0f1b396f74))
* **mods:** scan modpack folders and identify mods by storage key ([3058e37](https://github.com/EvilNick2/ron-mod-manager/commit/3058e3705ae271dd353506149b545648254207ed))
* **packs:** add and remove mods from a modpack in the side panel ([50d72bf](https://github.com/EvilNick2/ron-mod-manager/commit/50d72bf555e8e4f6496f200e1a31d8791ce9aa10))
* **packs:** apply, delete and export modpacks ([338bc93](https://github.com/EvilNick2/ron-mod-manager/commit/338bc93653723c813ed87a1969a4b935fa8ccc5c))
* **packs:** derive a stable pack id from preset.json ([23e781a](https://github.com/EvilNick2/ron-mod-manager/commit/23e781a7ef2e5f2e2f2409d14fb39bec841ebb40))
* **packs:** fork a linked pack into an editable copy ([f077776](https://github.com/EvilNick2/ron-mod-manager/commit/f077776400b91dae61f4e983419f6acecb4e2cb2))
* **packs:** hash mod files and build the per-mod hash ([6e4e67d](https://github.com/EvilNick2/ron-mod-manager/commit/6e4e67dc5f8a3b755eb9ecd0abf381e72cb4e542))
* **packs:** import modpacks into their own folder ([91bd27f](https://github.com/EvilNick2/ron-mod-manager/commit/91bd27f9e65357c6445baaa70062933517668a45))
* **packs:** modpacks group and import confirm in the sidebar ([3b219c5](https://github.com/EvilNick2/ron-mod-manager/commit/3b219c562045b6235fb8888d29c473bd0e5e96b0))
* **packs:** move mods into and out of a modpack ([f4e3889](https://github.com/EvilNick2/ron-mod-manager/commit/f4e388957dbb98c7adc760d577c7f0e15c05a306))
* **packs:** record pack source and publish state and lock linked packs ([9d0186d](https://github.com/EvilNick2/ron-mod-manager/commit/9d0186d3735e0bbe015b0b3f2ef40c262e83103f))
* **presets:** update saved presets when a modpack is imported ([4ea04d5](https://github.com/EvilNick2/ron-mod-manager/commit/4ea04d53d0de84d18795237743b09b79d4228f8b))
* **publish:** add the publish command and stop closing with unuploaded changes ([b65401c](https://github.com/EvilNick2/ron-mod-manager/commit/b65401cb4683516fc88b7ef1154bde441dc031a3))
* **publish:** publish a pack to Drive in updating mode with recovery ([163a045](https://github.com/EvilNick2/ron-mod-manager/commit/163a0452770f42a408b7598070eadace67928bfb))
* **publish:** require semantic versions and explain them in the version prompt ([f4eff79](https://github.com/EvilNick2/ron-mod-manager/commit/f4eff797535e5783c66202a16d89a344560b7095))
* **release:** upgrade aur publish system to new workflow ([ff8115d](https://github.com/EvilNick2/ron-mod-manager/commit/ff8115d691d3a93d4f21165c0701a7339cab269e))
* **subscribe:** add a modpack from a Drive link and check it for updates ([de81c44](https://github.com/EvilNick2/ron-mod-manager/commit/de81c4460144b0108bda5955c9df8d03af157640))
* **subscribe:** cancel an active pack download and leave the installed pack untouched ([73eb3a0](https://github.com/EvilNick2/ron-mod-manager/commit/73eb3a05392019aa549a883531dbff28c0f8eae0))
* **subscribe:** update a linked pack with only the changed files and verify it ([e54e373](https://github.com/EvilNick2/ron-mod-manager/commit/e54e373ff6ad761665b19f02f56fefc378342b5d))
* **ui:** add packs from links, update and verify them, fork and lock linked packs ([7df8554](https://github.com/EvilNick2/ron-mod-manager/commit/7df8554ac044c3e375a8aeb2b387fbdd4bf5898d))
* **ui:** show overall transfer progress in the progress modal ([4f501cc](https://github.com/EvilNick2/ron-mod-manager/commit/4f501cc947e45c64614f38205cf13d81a8b61344))
* **ui:** show pack progress in a modal and replace native dialogs with in-app ones ([b6c8ea5](https://github.com/EvilNick2/ron-mod-manager/commit/b6c8ea597b63bab18ac04a37521360084bc1b743))
* **ui:** sign in with Google, publish packs and warn about unuploaded changes ([0af46c1](https://github.com/EvilNick2/ron-mod-manager/commit/0af46c172370d3b16386da8517dec920ca41de3a))
* **workflow:** remove macos runner ([81d11c3](https://github.com/EvilNick2/ron-mod-manager/commit/81d11c37627e8f4295adc52e32cf418dbfcee338))


### Bug Fixes

* **deps:** replace removed rar-stream crate with unrar ([567d03a](https://github.com/EvilNick2/ron-mod-manager/commit/567d03a14d257099e352bfcc51c18bbf73f5f626))
* **drive:** send a user agent and show a readable message when Google blocks requests ([c0d58a6](https://github.com/EvilNick2/ron-mod-manager/commit/c0d58a654a2c3cd20c1e63339a8d1490f9ffebfd))
* **mods:** delete companion ucas and utoc files when removing an addon ([4faafa2](https://github.com/EvilNick2/ron-mod-manager/commit/4faafa2cd0f23ce1125a32f6130d0375a5227200))
* **mods:** deploy ucas and utoc files alongside their pak ([3ab4cee](https://github.com/EvilNick2/ron-mod-manager/commit/3ab4cee529289ba8b70df07f9a45320e4127e3be))
* **mods:** rebuild mod links from the Paks folder instead of the enabled flag ([c77e4cd](https://github.com/EvilNick2/ron-mod-manager/commit/c77e4cde74cbd01a013048f3d1301f27f4d27d5b))
* **packs:** harden deletes, rollbacks and Windows locked files from final review ([ccebc8e](https://github.com/EvilNick2/ron-mod-manager/commit/ccebc8e592c3050011542cf193b5bd43b539f67a))
* **publish:** guard every pack writer, check uploads and clean up interrupted transfers ([1d6d911](https://github.com/EvilNick2/ron-mod-manager/commit/1d6d9117fb5943af5c8db73477e5e104ae4a6680))
* **ui:** confirm destructive pack actions and keep pack state fresh ([ae85042](https://github.com/EvilNick2/ron-mod-manager/commit/ae85042e5497f89965bb45b2ca6e345cfd84e8ac))
* **ui:** show upload progress in the close guard ([7b30f75](https://github.com/EvilNick2/ron-mod-manager/commit/7b30f7539b2837a9088adb5a8f94d96f95d624fc))


### Performance Improvements

* **publish:** cache local file hashes so unchanged files are not read again ([bcf0dc3](https://github.com/EvilNick2/ron-mod-manager/commit/bcf0dc3cfc76f5e993ebdadf19732a4e9dd8013b))
* **publish:** trust a ready manifest instead of listing every Drive folder ([19f3373](https://github.com/EvilNick2/ron-mod-manager/commit/19f3373abfedb13bde3d110f9cb5d01ed17ccb32))
