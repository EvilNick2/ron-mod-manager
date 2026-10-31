# Changelog

## [0.6.0](https://github.com/EvilNick2/ron-mod-manager/compare/v0.5.0...v0.6.0) (2026-10-31)


### Features

* **mods:** install 7z archives and detect archive types by content ([5f8a233](https://github.com/EvilNick2/ron-mod-manager/commit/5f8a2335ad3dcb9eb259080b8ed8e0d30d35ee99))
* **packs:** add the pack server client ([0c0d7b8](https://github.com/EvilNick2/ron-mod-manager/commit/0c0d7b8e51faa4e5e82ca0195cf02c652ac4d38c))
* **packs:** create a modpack from the sidebar ([446f15b](https://github.com/EvilNick2/ron-mod-manager/commit/446f15bdfdb3309c472c4cffd5f5a995b7aae95b))
* **packs:** generate changelogs from pack changes ([d982f99](https://github.com/EvilNick2/ron-mod-manager/commit/d982f9973ad1b09ae8960f70690e51c29b61601c))
* **packs:** manage pack access and remove google drive ([6b014b5](https://github.com/EvilNick2/ron-mod-manager/commit/6b014b5d0b0e2d0a86c7847a70849abd128c0fff))
* **packs:** publish, update and manage working copies of a pack ([d8d9230](https://github.com/EvilNick2/ron-mod-manager/commit/d8d92308b9633ff4b48a9d48a00c3d7aafdbe3ae))
* **packs:** sign in to pack servers with discord ([0812e91](https://github.com/EvilNick2/ron-mod-manager/commit/0812e9148c4047b35b0c7d2b2d80a49f49ecbb9c))
* **publish:** publish packs to pack servers ([36b2dbc](https://github.com/EvilNick2/ron-mod-manager/commit/36b2dbc122a439de740154575c2731ea5af3683d))
* **server-client:** talk to pack editors, holds and base versions ([ab75c14](https://github.com/EvilNick2/ron-mod-manager/commit/ab75c14dfd80d2686c844e62e1e6af668d04a167))
* **server:** add sessions and the auth guard ([bc91bc2](https://github.com/EvilNick2/ron-mod-manager/commit/bc91bc226f8fcbb0acb096b2ecd1a4e78dc56d17))
* **server:** add uploader invites and the people cap ([1302b69](https://github.com/EvilNick2/ron-mod-manager/commit/1302b69240ca6e1e8c90ee70438cd62eedd8e7ac))
* **server:** clean up orphan files on a schedule ([0f9c5cd](https://github.com/EvilNick2/ron-mod-manager/commit/0f9c5cd3fc135630fa5544737acc2f8744cfbadf))
* **server:** hold a pack while it publishes and refuse stale publishes ([8fb4b83](https://github.com/EvilNick2/ron-mod-manager/commit/8fb4b8342857f52232c4b06008bba1c51992519e))
* **server:** keep a changelog per pack version ([93d2201](https://github.com/EvilNick2/ron-mod-manager/commit/93d22011fdd5c79203c358e534ab380296cf3dca))
* **server:** let a pack's creator add editors ([8cf3912](https://github.com/EvilNick2/ron-mod-manager/commit/8cf39120b5bcdcf54bf85f83563c65fc4b92ab2a))
* **server:** let uploaders and the owner manage access ([f21ec6d](https://github.com/EvilNick2/ron-mod-manager/commit/f21ec6d95e497686e4c3e47494dd56bd9c418040))
* **server:** return published_at on commit and cap history versions ([241b3b0](https://github.com/EvilNick2/ron-mod-manager/commit/241b3b08c5a6ebc590180bdb715d6ba14ea27af7))
* **server:** scaffold the pack server worker ([a094d8e](https://github.com/EvilNick2/ron-mod-manager/commit/a094d8eb877238adc6cd396cb9bfede170a4222c))
* **server:** serve pack files to people with access ([a11ae10](https://github.com/EvilNick2/ron-mod-manager/commit/a11ae10a7831ad5d1878b86d066f2adaf6be7735))
* **server:** sign in with discord ([09c8875](https://github.com/EvilNick2/ron-mod-manager/commit/09c8875ad3f0e135450bd53966b2ef36c65390c8))
* **server:** stop cleared publishes and let the owner become an editor ([3db49f6](https://github.com/EvilNick2/ron-mod-manager/commit/3db49f676a2b980abf302b09d25777b466f61ce6))
* **server:** take publishes under the storage cap ([1d3c818](https://github.com/EvilNick2/ron-mod-manager/commit/1d3c818d6e90232edd1416e67c010a50e72a40e1))
* **subscribe:** install and update packs from pack servers ([b727f93](https://github.com/EvilNick2/ron-mod-manager/commit/b727f93502110d15bda3ac73e96dad3dd4013aea))
* **subscribe:** switch an installed pack to another link ([aa2d155](https://github.com/EvilNick2/ron-mod-manager/commit/aa2d155c2436d3a0184c031608ba253b37bec973))
* **ui:** manage pack editors, accept invites in settings and warn before replacing changes ([4a64605](https://github.com/EvilNick2/ron-mod-manager/commit/4a646051cf98ce107c592d74f62a0f84c719004a))
* **ui:** show pack details and changelogs ([c89a7ae](https://github.com/EvilNick2/ron-mod-manager/commit/c89a7ae42e3ad66b29d4c61360c11968f99195e0))
* **ui:** sign in to pack servers and manage pack access ([cee023b](https://github.com/EvilNick2/ron-mod-manager/commit/cee023b0c7b0ee583a6041b369b6f065c1745de3))


### Bug Fixes

* **mods:** keep the installed copy when an archive fails to extract ([fbc2f2b](https://github.com/EvilNick2/ron-mod-manager/commit/fbc2f2b71c50f7edfdec4cd11868ef78cc17357b))
* **mods:** keep the installed version when checking nexus for updates ([2a13cc5](https://github.com/EvilNick2/ron-mod-manager/commit/2a13cc5589b9353f1d287bd9fab39de45cfe7721))
* **packs:** harden switching, sign-in and server requests ([e3b2fe1](https://github.com/EvilNick2/ron-mod-manager/commit/e3b2fe1404968d3256768625654324aa8325f6dd))
* **packs:** refresh roles after invites and allow switching servers ([e5959bb](https://github.com/EvilNick2/ron-mod-manager/commit/e5959bbe5d9e6d7866b20c829648c8afe2001194))
* **server:** close storage cap and invite gaps from review ([d493a4c](https://github.com/EvilNick2/ron-mod-manager/commit/d493a4cf2d0fefc5cc47c340ec80e19c13f21294))
* **ui:** make dropdowns readable on webkitgtk ([af7c8f1](https://github.com/EvilNick2/ron-mod-manager/commit/af7c8f1b339994152bcc5f9675f8268adab6c77b))
* **ui:** move pack export into the more menu ([4fdd091](https://github.com/EvilNick2/ron-mod-manager/commit/4fdd09147a4a366971ba9e9573d14def0a34b671))
* **ui:** offer the invite link only on a pack's first publish ([381a235](https://github.com/EvilNick2/ron-mod-manager/commit/381a2350174a0dbc246fac833103fb0a31b412f1))

## [0.5.0](https://github.com/EvilNick2/ron-mod-manager/compare/v0.4.1...v0.5.0) (2026-10-24)


### Features

* **packs:** keep verified downloads so a failed add or update resumes ([b8522bc](https://github.com/EvilNick2/ron-mod-manager/commit/b8522bcf58a4ff190ecdf764affc6eadb48b5ab4))
* **packs:** show progress when importing a modpack zip ([e65d563](https://github.com/EvilNick2/ron-mod-manager/commit/e65d563dc4b7c6ab654711e7b2a86619f127f329))


### Bug Fixes

* **drive:** wait and retry when Google blocks pack downloads ([eea2363](https://github.com/EvilNick2/ron-mod-manager/commit/eea23635fa2f3d6a414033ab1453cb9b0abf4bc0))
* **linux:** keep webkit's gpu renderer on nvidia wayland ([b533f5f](https://github.com/EvilNick2/ron-mod-manager/commit/b533f5f162fbedc080bf9b7765ea83917569f942))

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
