# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.1](https://github.com/noirbizarre/over/compare/0.6.0..0.6.1) - 2026-11-23

### 🐛 Bug Fixes

- **cli** Improve help, key management, and history defaults ([#356](https://github.com/noirbizarre/over/issues/356)) - ([0e5df05](https://github.com/noirbizarre/over/commit/0e5df05b278234d39b83331e4b9cc324733eeb0b))
- **cli** Restore colored help output - ([e27076b](https://github.com/noirbizarre/over/commit/e27076b1f75cbeded8ad784cdfcecbff30978d5d))
- **crypt** Choose a user/key before interactive acquisition ([#358](https://github.com/noirbizarre/over/issues/358)) - ([8de3f97](https://github.com/noirbizarre/over/commit/8de3f97b90945cd15796a94b43a84da9fada708e))
- **crypt** Report precise sanitized decryption failures ([#357](https://github.com/noirbizarre/over/issues/357)) - ([463556d](https://github.com/noirbizarre/over/commit/463556da0dbbecf2f620ca65ed8fa62810846310))
- **crypt** Allow revocation with reused passphrases - ([663d941](https://github.com/noirbizarre/over/commit/663d9414c5e02f8d3f5085ea29f1280270c2a0d0))
- **crypt** Identify failing native secret filter paths - ([a3bb626](https://github.com/noirbizarre/over/commit/a3bb6269786302f9bdc9e1a1ea1bfe261d81258f))
- **log** Fall back to repository history for ambiguous context - ([14de04c](https://github.com/noirbizarre/over/commit/14de04cb2090611272deab3abd1a350e7d2160a3))
- **user** Accept combined user/key references for key removal - ([b7edb03](https://github.com/noirbizarre/over/commit/b7edb0394c5d2d462ea165cd8704620c9a300fb4))
- **user** Accept positional key names - ([6f84cc5](https://github.com/noirbizarre/over/commit/6f84cc5b509284b0a7f9703a5ac4c40aca6bd0ea))
- Align overlay inspection, application, and documentation ([#359](https://github.com/noirbizarre/over/issues/359)) - ([29c2d5a](https://github.com/noirbizarre/over/commit/29c2d5a8849a8ad50b791aa27e05ca5460005df5))
- Preserve locked inspection without local crypt state - ([6786771](https://github.com/noirbizarre/over/commit/6786771f69b114f99e018c59ed79d7ebdea66839))
- Preserve unassociated directories after checkout conflicts - ([c89a7cd](https://github.com/noirbizarre/over/commit/c89a7cdf0da77d13a5c261400d89b1b5943d61e6))
- Reject symlink replacements in ordinary generated-checkout inspection - ([04c642c](https://github.com/noirbizarre/over/commit/04c642cfebba3c76f9fb78fe3d21ed97c3e9a406))
- Align diff with managed symlink identity - ([5836345](https://github.com/noirbizarre/over/commit/5836345298575670acec428ff91c5c24a4c89c9c))
- Resolve managed dangling mappings consistently - ([8d1426c](https://github.com/noirbizarre/over/commit/8d1426ca9d84f8475a109b04be88e0bcf3969625))
- Keep protected inspection read-only - ([31c6836](https://github.com/noirbizarre/over/commit/31c6836a236e2221bea6c511b89e07c14b1c7328))
- Honor target access when enumerating diffs - ([40466f0](https://github.com/noirbizarre/over/commit/40466f0a830412a4e25733a718bd06e7de9bc231))
- Enforce intermediate checkout directory ownership - ([1d16974](https://github.com/noirbizarre/over/commit/1d16974c38659f8e6d95f081c04acc753fb8e291))
- Warn about unknown sidecar attributes - ([038fa09](https://github.com/noirbizarre/over/commit/038fa09019bdf4ef5bb82fa4ef01fc761574440d))
- Preserve Git semantics for ordinary generated-checkout siblings - ([43aca58](https://github.com/noirbizarre/over/commit/43aca589dc7ac7329317bd49d0c56e5933ff1db3))
- Report overlay completion after deferred application - ([c0ed1d3](https://github.com/noirbizarre/over/commit/c0ed1d3ecd9a1c78553a2e4e6d6e962877335756))
- Inspect staged additions in generated checkouts - ([ed6f27b](https://github.com/noirbizarre/over/commit/ed6f27bea6121afc8cbe98a44d3cc29b691dfc8d))

### 📚 Documentation

- **cli** Describe user and key commands - ([e97dd28](https://github.com/noirbizarre/over/commit/e97dd28f348e634cc8d5eb13bd3696936b2b4f3e))
- Align doctor report section descriptions - ([7d1e067](https://github.com/noirbizarre/over/commit/7d1e0670227df7ca39439f38f6b3115a6bd4f75f))
- Clarify materializer operation dispatch - ([76936e7](https://github.com/noirbizarre/over/commit/76936e790cf2d9b344617baf60f131e67525b2d9))
- Clarify documentation workflow triggers - ([536f514](https://github.com/noirbizarre/over/commit/536f51403906b6d47818fe4cd9854ae020077bb0))
- Correct partial permission rationale - ([42ad136](https://github.com/noirbizarre/over/commit/42ad1363cd8d7a87963069e2303caf153e307fdb))
- Clarify commit contextual selection - ([a17155c](https://github.com/noirbizarre/over/commit/a17155ccea058aee5e4da2cded49518b61995f34))
- Clarify encryption metadata in commit scope - ([5b78921](https://github.com/noirbizarre/over/commit/5b7892197bf8ccde5fcc713659643e85749cf3d2))

### 🧪 Tests

- Normalize path assertions across platforms - ([930bfab](https://github.com/noirbizarre/over/commit/930bfab188d8e0ee8c3df7a57574fd17da84fe74))
- Renormalize tracked files after configuring clean filters - ([4aebe45](https://github.com/noirbizarre/over/commit/4aebe45ad1570c23dd11e0cf4da018f360f73caa))

### 🏗️ Build

- **deps** Bump the rust-dependencies group with 4 updates ([#350](https://github.com/noirbizarre/over/issues/350)) - ([d033e0b](https://github.com/noirbizarre/over/commit/d033e0b1996eec57fbab2d0b3538b3d19da44a51))
- **deps** Bump jsonschema from 0.52.0 to 0.61.2 ([#351](https://github.com/noirbizarre/over/issues/351)) - ([3e05e66](https://github.com/noirbizarre/over/commit/3e05e66a2eb97da3bcc7e1c87ab729879b279a7e))
- **deps** Bump noyalib from 0.0.60 to 0.0.62 ([#352](https://github.com/noirbizarre/over/issues/352)) - ([e520199](https://github.com/noirbizarre/over/commit/e520199c37e82c19cdde6c77a0bec9786d12e301))

## [0.6.0](https://github.com/noirbizarre/over/compare/0.5.1..0.6.0) - 2026-11-20

### 💫 Features

- **apply** Support skipping overlays by name or glob ([#342](https://github.com/noirbizarre/over/issues/342)) - ([a0d4a00](https://github.com/noirbizarre/over/commit/a0d4a00d3554a280abc6f1e0a7c710a1c2bd8e66))
- **devcontainer** Support read-only mounted checkouts ([#347](https://github.com/noirbizarre/over/issues/347)) - ([d8c5999](https://github.com/noirbizarre/over/commit/d8c5999e50ca2c183f6535a2027abb1c2a2382d4))
- **secrets** Read native secrets from bare repositories ([#344](https://github.com/noirbizarre/over/issues/344)) - ([f76258e](https://github.com/noirbizarre/over/commit/f76258eb8a9e1e12d73130bc196af02ba038ee7a))
- **secrets** Read locked secrets without persisting an unlock ([#343](https://github.com/noirbizarre/over/issues/343)) - ([480c408](https://github.com/noirbizarre/over/commit/480c4084853829f5b1866b778e0634bb6425a59a))

### 🐛 Bug Fixes

- **apply** Finalize published directory metadata on failure - ([4fc7ecd](https://github.com/noirbizarre/over/commit/4fc7ecdc4d7587e7c59410825135a01a329fee73))
- **apply** Finalize scaffold metadata after deferred partial writes - ([abd1bb0](https://github.com/noirbizarre/over/commit/abd1bb0991d50b29986f5554b9b85d16221c10d9))
- **checkout** Retry generated snapshot ref lock contention ([#339](https://github.com/noirbizarre/over/issues/339)) - ([10a20de](https://github.com/noirbizarre/over/commit/10a20de2e8da1fc02b6801568d4ff14646b33222))
- **crypt** Repair read-only destination permissions without rewriting - ([917c8f8](https://github.com/noirbizarre/over/commit/917c8f8b49219fee4f255aa5c7a137dc5e448ec2))
- **git** Tolerate index cache refreshes during commit preparation ([#337](https://github.com/noirbizarre/over/issues/337)) - ([f1c7bd6](https://github.com/noirbizarre/over/commit/f1c7bd62e60c8ce3223dfb7959662139e3e47596))
- **partial** Reject overlapping selected block ownership - ([3903362](https://github.com/noirbizarre/over/commit/3903362fadca8f771b85d6e3940bcb2ca713d8e8))
- **sync** Report missing targets before stale mapping checks - ([6a7bc32](https://github.com/noirbizarre/over/commit/6a7bc324203cd8fbcd3000da11076011b254f847))
- **sync** Handle local edits after virtual checkout refresh - ([f764369](https://github.com/noirbizarre/over/commit/f764369844557385aa55eff04cf1dcc133c54a27))
- Protect rendered source metadata and align checkout behavior ([#348](https://github.com/noirbizarre/over/issues/348)) - ([983493a](https://github.com/noirbizarre/over/commit/983493a65de40420c2ad89daac9ce5b14b016a58))
- Label rendered diffs as current to desired - ([86bac98](https://github.com/noirbizarre/over/commit/86bac98eec241ed64c73ca82e2d972ad90f11422))
- Correct sync conflict recovery guidance - ([25b0aae](https://github.com/noirbizarre/over/commit/25b0aaee28eb04773a763c18db063e1add5e2fe2))
- Require applied generated checkouts before sync - ([69557f6](https://github.com/noirbizarre/over/commit/69557f668fb328675057a84f2fab23d6089c41e9))
- Protect source git metadata from rendered destinations - ([2a8ffc7](https://github.com/noirbizarre/over/commit/2a8ffc768f024bb47a7849e71b930304bc994b31))
- Reconcile partial ownership, permission repair, sync, and documentation ([#346](https://github.com/noirbizarre/over/issues/346)) - ([f36843c](https://github.com/noirbizarre/over/commit/f36843cf4e4b281aec17ed76e9fd9d9fb34bea41))

### ⚡ Performance

- **crypt** Avoid PowerShell for Windows ACL operations ([#349](https://github.com/noirbizarre/over/issues/349)) - ([6999c13](https://github.com/noirbizarre/over/commit/6999c13393cb7dfcdafa561b73786d562191f06e))

### 🔨 Refactor

- **secrets** Store authenticated binary values ([#340](https://github.com/noirbizarre/over/issues/340)) - ([f0220d8](https://github.com/noirbizarre/over/commit/f0220d8d135fd82dc07f1c39bf0ea8cf9c8b4cdf))

### 📚 Documentation

- **config** Select a directory in the checkout example - ([c8570a1](https://github.com/noirbizarre/over/commit/c8570a1b32f71afa4c6beae44b97513526d1d711))
- **config** Document strict permissions object fields - ([a67e0c0](https://github.com/noirbizarre/over/commit/a67e0c02e75a2f8af124f6eb39e7b823056059c2))
- **crypt** Clarify native-secret diff bypass after rotation - ([1a68eff](https://github.com/noirbizarre/over/commit/1a68eff457e3594513cb1e8a65a0cb99953f8bea))
- **crypt** Use the native-secret diff driver in policy examples - ([163fb7c](https://github.com/noirbizarre/over/commit/163fb7c6720f50de8082fd811dbf1969b13c69f6))
- **crypt** Clarify locked one-shot secret reads - ([a03dbc1](https://github.com/noirbizarre/over/commit/a03dbc1bc0e3fcca24d8cc1beb3b96b87cbbd80c))
- **restore** Describe generated writes and metadata preservation - ([206b339](https://github.com/noirbizarre/over/commit/206b33901c3bab17704bbf466e08204225c9a646))
- **selection** Distinguish commit context from path inference - ([bda98af](https://github.com/noirbizarre/over/commit/bda98af064c9e2bd3a4339da2f7a10f767df24d2))
- **status** Align exit codes with inspection failures - ([bb28995](https://github.com/noirbizarre/over/commit/bb2899558d749e74b8b6258a77c7649bda8d563b))
- Note strict permissions object validation - ([1cfbf35](https://github.com/noirbizarre/over/commit/1cfbf35cf178fd9dc7be0770713beab7dc812157))
- Correct symlink materializer responsibilities - ([bf43f66](https://github.com/noirbizarre/over/commit/bf43f66513747e53199249811e42d8ed1ad46bde))
- Distinguish mode and ownership policies - ([de14143](https://github.com/noirbizarre/over/commit/de14143bb95ebc1c91b0d2d54bf1d562910be47f))
- Distinguish generated restore platform support - ([7f5e486](https://github.com/noirbizarre/over/commit/7f5e486ada7e2b97bad6b09761d665dfa115c0f1))
- Clarify legacy sync abort reset semantics - ([5d7c8f7](https://github.com/noirbizarre/over/commit/5d7c8f75521508a6727a3ef232aa8df6c1e846ef))
- Pass filename context in manual filter setup - ([55e0cfc](https://github.com/noirbizarre/over/commit/55e0cfc9d9011d3f6e7f9e028f0e2ca3f23b46e4))

### 🧪 Tests

- **sync** Expect pending mapped edits to report blocked - ([b269118](https://github.com/noirbizarre/over/commit/b2691187316654d2f9a4c899abed0b0109b8d4db))
- Define rendered diff symlink inspection policy - ([10ccb17](https://github.com/noirbizarre/over/commit/10ccb17904cdfa14431e098a35e7563935522d62))

### 🧹 Chores

- **template** Merge latest rust.tpl updates ([#345](https://github.com/noirbizarre/over/issues/345)) - ([5d1fd67](https://github.com/noirbizarre/over/commit/5d1fd6737ab37bb8edec281848d37fb05b4d5d05))
- **template** Merge latest rust.tpl updates - ([98ec67b](https://github.com/noirbizarre/over/commit/98ec67bcbea07a19c7f315f87aed5a497840b03a))

### Tpl

- Render rust at main - ([80f38aa](https://github.com/noirbizarre/over/commit/80f38aae4c79ead7ff0742a165aaf711508d3f7a))

## [0.5.1](https://github.com/noirbizarre/over/compare/0.5.0..0.5.1) - 2026-11-17

### 🐛 Bug Fixes

- **cli** Require confirmation for removal and discard commands ([#331](https://github.com/noirbizarre/over/issues/331)) - ([838701b](https://github.com/noirbizarre/over/commit/838701b5f6b7e6d8f316690a314e94d5c6142a64))
- **crypt** Accept filter control packets without trailing newlines ([#330](https://github.com/noirbizarre/over/issues/330)) - ([7d67f61](https://github.com/noirbizarre/over/commit/7d67f611be5e1d8eac5411f2599baee075952805))
- **status** Limit reports to overlay-managed state ([#332](https://github.com/noirbizarre/over/issues/332)) - ([72ea23f](https://github.com/noirbizarre/over/commit/72ea23fb0f51c560df876443d3688177d7ab934c))

## [0.5.0](https://github.com/noirbizarre/over/compare/0.4.0..0.5.0) - 2026-11-17

### 💫 Features

- **crypt** Commit split secrets and access metadata ([#323](https://github.com/noirbizarre/over/issues/323)) - ([dbb1962](https://github.com/noirbizarre/over/commit/dbb1962860dad4abd154f4fd065b89715157e48d))
- **sudo** Add elevated overlays, hooks, and ownership ([#325](https://github.com/noirbizarre/over/issues/325)) - ([aacb182](https://github.com/noirbizarre/over/commit/aacb182a233b45b0da353b54075ffe13e1aa2e5d))
- **templates** Add yesno filter ([#324](https://github.com/noirbizarre/over/issues/324)) - ([d51ced3](https://github.com/noirbizarre/over/commit/d51ced30d6204be3e35d64b18a1be35e27f4d2f0))

### 🐛 Bug Fixes

- **checkout** Allow sources inside shared target directories ([#326](https://github.com/noirbizarre/over/issues/326)) - ([856ce04](https://github.com/noirbizarre/over/commit/856ce044efd0353a7fa59ce8a3b7867b4d2fff22))
- **crypt** Confirm new user key passphrases ([#319](https://github.com/noirbizarre/over/issues/319)) - ([4ae4083](https://github.com/noirbizarre/over/commit/4ae408345089ce627aa2b8d767f9c57bed250513))
- **crypt** Avoid redundant bootstrap attribute guards ([#318](https://github.com/noirbizarre/over/issues/318)) - ([2c406ba](https://github.com/noirbizarre/over/commit/2c406baddedbcf091f30680d4d43b3f4245524f3))
- **data** Honor OVER_TARGET when resolving exported context - ([a3542f5](https://github.com/noirbizarre/over/commit/a3542f5d5544619ca905e29fac18f6e0bd7aaa8f))
- **git** Redact secret-bearing provisioning diagnostics - ([12e7d6f](https://github.com/noirbizarre/over/commit/12e7d6fcd5a7dca38be8ec8f83e835e4d55afbe7))
- **git** Validate worktree destinations before provisioning - ([37eee8a](https://github.com/noirbizarre/over/commit/37eee8a839516f88cb3a37bfffc7206c35193713))
- **new** Resolve absorption targets through prospective overlays - ([cf825be](https://github.com/noirbizarre/over/commit/cf825be45c3200896410b284b1b1679a8d493448))
- **new** Confine descriptor creation to the overlay repository - ([c902d52](https://github.com/noirbizarre/over/commit/c902d52f746f5ca3761e24f8f3902ac753eb1413))
- **partial** Reject inputs that break block round trips - ([10f3451](https://github.com/noirbizarre/over/commit/10f3451551d72a964c77a4037fa962b2d38d65bb))
- **plan** Guard metadata repairs against stale destinations - ([f73eda1](https://github.com/noirbizarre/over/commit/f73eda1a922961c601898d718c53ec6bc67f06ee))
- **status** Reject unresolved HEAD for declared revisions - ([4d548f4](https://github.com/noirbizarre/over/commit/4d548f4bb6ecea930e1ef29418cb48e6949f33d4))
- **sudo** Avoid macOS helper shutdown race ([#327](https://github.com/noirbizarre/over/issues/327)) - ([a4a97ad](https://github.com/noirbizarre/over/commit/a4a97ad7c138b3bc8157fcd411bf94d7ee20f173))
- **sync** Propagate preview and prompt policy to target access - ([653095d](https://github.com/noirbizarre/over/commit/653095dc44661c49ab8d24de0e74292950384f28))
- **unapply** Preserve exclusions for retained virtual checkouts - ([78be9c8](https://github.com/noirbizarre/over/commit/78be9c8a5f2744b02b9d70eeb15e585e85c0aef5))
- Reconcile overlay safety and documentation contracts ([#329](https://github.com/noirbizarre/over/issues/329)) - ([7d450ac](https://github.com/noirbizarre/over/commit/7d450accd6f202c73f600e39ca258d03715220b5))

### 📚 Documentation

- **crypt** Reconcile administrative policy with current layout - ([6990cd3](https://github.com/noirbizarre/over/commit/6990cd32a6acf8c99e2429af6df5c3b96e3b16af))
- **init** Qualify bootstrap hooks for locked checkouts - ([b867c4d](https://github.com/noirbizarre/over/commit/b867c4df4e49a95d11e7d669ce3b9297bca66d37))
- **rules** Include sudo-only assignments in validation guidance - ([4016e2b](https://github.com/noirbizarre/over/commit/4016e2b043aedb56ab037e89b011359d3a689e5d))
- **safety** Explain sudo authentication during previews - ([b61b3bf](https://github.com/noirbizarre/over/commit/b61b3bfe7118bf6486b63c7c7bf77b6cff4bafc9))
- **sync** Correct legacy abort recovery guidance - ([c683cc4](https://github.com/noirbizarre/over/commit/c683cc475720fce12acb68eb1eda7d12390a7cce))
- **unapply** Align removal comments with checkout disposal - ([ae045ff](https://github.com/noirbizarre/over/commit/ae045ff6de6dbd190561e2810671e8c027e41c82))

### 🏗️ Build

- **docker**  🚨 **breaking** Reuse binaries and simplify Feature packaging ([#316](https://github.com/noirbizarre/over/issues/316)) - ([b1e3195](https://github.com/noirbizarre/over/commit/b1e3195e698c3091de63665e60155bd2654b3482))

### 🔧 CI

- **context7** Refresh documentation and index tagged versions ([#328](https://github.com/noirbizarre/over/issues/328)) - ([5bf8530](https://github.com/noirbizarre/over/commit/5bf853080d155366597d2f2544ddeefbb99c3f7b))

## [0.4.0](https://github.com/noirbizarre/over/compare/0.3.0..0.4.0) - 2026-11-15

### 💫 Features

- **crypt** Detect repository encryption before checkout ([#306](https://github.com/noirbizarre/over/issues/306)) - ([71ecb1f](https://github.com/noirbizarre/over/commit/71ecb1f4f6f5e678fa88280c7c8fcfe06fd52b95))
- **data**  🚨 **breaking** Separate host and overlay data scopes ([#305](https://github.com/noirbizarre/over/issues/305)) - ([631ef92](https://github.com/noirbizarre/over/commit/631ef92394ff819af99f0680a6b3a7a594cb383b))
- **init** Unlock encrypted repositories before initial checkout ([#312](https://github.com/noirbizarre/over/issues/312)) - ([1ef947b](https://github.com/noirbizarre/over/commit/1ef947b592216ae5ae8006f56033b6d42ffe8ce9))
- **overlays**  🚨 **breaking** Separate host context from deployment defaults ([#311](https://github.com/noirbizarre/over/issues/311)) - ([744251d](https://github.com/noirbizarre/over/commit/744251db498e62d59356384945225de4cf4365f6))

### 🐛 Bug Fixes

- **add** Preserve symlink referents in Git ingestion - ([236314a](https://github.com/noirbizarre/over/commit/236314ac6e8699a808cfc9a4fb6e5a36901af90f))
- **crypt** Make locked Git operations safe and retry interrupted unlocks ([#310](https://github.com/noirbizarre/over/issues/310)) - ([0c4f56b](https://github.com/noirbizarre/over/commit/0c4f56b3de476956999188e4e88388812ebca65d))
- **crypt** Restore authenticated Git textconv diffs ([#298](https://github.com/noirbizarre/over/issues/298)) - ([fac2834](https://github.com/noirbizarre/over/commit/fac2834c06873bdb71c6bf3d67cd006f835738ae))
- **deps** Support system OpenSSL 4.x ([#288](https://github.com/noirbizarre/over/issues/288)) - ([2acbc0c](https://github.com/noirbizarre/over/commit/2acbc0c3ecbf632fa1a354a8de2a9f7fc252d9ab))
- **doctor** Scope checkout records to their source collection - ([ae32cd1](https://github.com/noirbizarre/over/commit/ae32cd1ada704896b793d4faec9b9fe25608115c))
- **git** Resolve declared tags consistently - ([16b119f](https://github.com/noirbizarre/over/commit/16b119f1a9ab852b32fb0c593040b82c3592614b))
- **restore** Retain ownership of deleted checkout roots - ([7d101a6](https://github.com/noirbizarre/over/commit/7d101a6d9b0355c73c36c01bef4892cb4782916d))
- **status** Retain executable modes in checkout conflicts - ([a7f5ce9](https://github.com/noirbizarre/over/commit/a7f5ce9aa51f8940e464549d20e0e6004d86fb76))
- **sync** Verify the snapshot approved before committing - ([6198a29](https://github.com/noirbizarre/over/commit/6198a2944529ec0b18201f98f86c1bf9aa3a13a3))
- Reconcile overlay behavior and documentation inconsistencies ([#314](https://github.com/noirbizarre/over/issues/314)) - ([8dc3bd7](https://github.com/noirbizarre/over/commit/8dc3bd7705f912dc1a01aedff5e917aac0c304eb))

### 📚 Documentation

- **ci** Correct the Clippy linking rationale - ([ed6dd21](https://github.com/noirbizarre/over/commit/ed6dd214fcfe6a4cc9d0c5344f1e0ce4e1d7921f))
- **contributing** Clarify template update and merge steps - ([bc4418e](https://github.com/noirbizarre/over/commit/bc4418e9c62fa71a64e787978d17440dd678e5e1))
- **crypt** Document legacy encryption migration - ([2f26cb0](https://github.com/noirbizarre/over/commit/2f26cb09e240419b9c83ebaf2ab8ce0af68b3f4d))
- **data** Correct contextual selection precedence - ([da14c5b](https://github.com/noirbizarre/over/commit/da14c5bd517172a2d1c2d0a870ed51902247340d))
- **desired** Refresh discovery and permission boundaries - ([cbbae19](https://github.com/noirbizarre/over/commit/cbbae19b9978e62cd7ec2162948e0c553c4e27ab))
- **diff** Explain locked ciphertext passthrough - ([817f2e5](https://github.com/noirbizarre/over/commit/817f2e5c2bd73fc23002bfb32d380e290428e162))
- **install** Clarify explicit dependency package access - ([deba7c6](https://github.com/noirbizarre/over/commit/deba7c6ce25655d1dbb27f29a0e96e7817a30a63))
- Reorganize guides and command references ([#290](https://github.com/noirbizarre/over/issues/290)) - ([95734b1](https://github.com/noirbizarre/over/commit/95734b1fb2770060050caa5ccf872c864cb4c340))

### 🏗️ Build

- **deps** Bump the rust-dependencies group across 1 directory with 6 updates ([#301](https://github.com/noirbizarre/over/issues/301)) - ([9ace00e](https://github.com/noirbizarre/over/commit/9ace00ed9dc960f126c4d254fc2592e2adce0c45))
- **deps** Bump aes from 0.8.4 to 0.9.4 ([#293](https://github.com/noirbizarre/over/issues/293)) - ([fbdf1ff](https://github.com/noirbizarre/over/commit/fbdf1ff1734a54da687d30847fe34bb0473571db))
- **deps** Bump noyalib from 0.0.58 to 0.0.60 ([#292](https://github.com/noirbizarre/over/issues/292)) - ([b593cc1](https://github.com/noirbizarre/over/commit/b593cc1da8e2d6c29d179ca65104b0298902ebff))
- **deps** Bump jsonschema from 0.46.10 to 0.52.0 ([#294](https://github.com/noirbizarre/over/issues/294)) - ([2cbcb9b](https://github.com/noirbizarre/over/commit/2cbcb9b0d4b221393404d3443b5a1bba93ca0d6d))
- **deps** Bump getrandom from 0.3.4 to 0.4.2 ([#296](https://github.com/noirbizarre/over/issues/296)) - ([f545036](https://github.com/noirbizarre/over/commit/f54503641d2f80b6036b0b732f18ecebe1787711))
- **deps** Bump aes-siv from 0.7.0 to 0.8.0 ([#297](https://github.com/noirbizarre/over/issues/297)) - ([c3b60cf](https://github.com/noirbizarre/over/commit/c3b60cf3d89d29a75cb2c370eaeedd0a4bc3460c))

### 🔧 CI

- **deps** Bump actions/setup-node from 6 to 7 in the actions group ([#295](https://github.com/noirbizarre/over/issues/295)) - ([aca2089](https://github.com/noirbizarre/over/commit/aca20897a14be23c9a87716c0d7046b929177d06))
- **docs** Always build docs on pull requests ([#309](https://github.com/noirbizarre/over/issues/309)) - ([f6afb84](https://github.com/noirbizarre/over/commit/f6afb84f01efe8efea6066adbef48e589fb49c9d))
- Pull LLVM profile collision fix from rust.tpl ([#308](https://github.com/noirbizarre/over/issues/308)) - ([0463152](https://github.com/noirbizarre/over/commit/0463152d8ece205b4b29e3cd116327e7ae41b3fc))
- Pull LLVM profile collision fix from rust.tpl - ([b773167](https://github.com/noirbizarre/over/commit/b773167440970b2da1f5ca2264a8c3b8d7104dea))

### Tpl

- Render rust at main - ([840fee7](https://github.com/noirbizarre/over/commit/840fee7217a5128d8f33b6c2247c0bdcab87a323))

## [0.3.0](https://github.com/noirbizarre/over/compare/0.2.1..0.3.0) - 2026-11-14

### 💫 Features

- **checkout** Compose disjoint overlays at shared targets ([#274](https://github.com/noirbizarre/over/issues/274)) - ([0ba794f](https://github.com/noirbizarre/over/commit/0ba794f1c833bc205731d0d352f694035c0be132))
- **crypt** Support Age plugins and independent access keys ([#285](https://github.com/noirbizarre/over/issues/285)) - ([d8f5b31](https://github.com/noirbizarre/over/commit/d8f5b3196f0db7cf7a40d7c8575919ba8250dcf0))
- **devcontainer** Support comma-separated overlay composition ([#278](https://github.com/noirbizarre/over/issues/278)) - ([8ef6a81](https://github.com/noirbizarre/over/commit/8ef6a81d6ca6a0d9611dbeb134fd6de5afb1d117))
- **init** Offer virtual checkout as the repository default ([#273](https://github.com/noirbizarre/over/issues/273)) - ([cb140f5](https://github.com/noirbizarre/over/commit/cb140f5c607cd64dc9ebe7850bfb267df0947a6a))
- **templates** Reverse patch rendered edits into source templates ([#281](https://github.com/noirbizarre/over/issues/281)) - ([65cde9c](https://github.com/noirbizarre/over/commit/65cde9c4ddf624c8293d01234c9f95e8d1a5230a))
- **templates** Render files through desired state ([#279](https://github.com/noirbizarre/over/issues/279)) - ([3aa33e7](https://github.com/noirbizarre/over/commit/3aa33e71f35c4fea83bd3a061d9f41d717b8393c))

### 🐛 Bug Fixes

- **checkout** Preserve filesystem safeguards in generated checkouts - ([336ff2f](https://github.com/noirbizarre/over/commit/336ff2fd4d91d91ff57a189dd2ad098f9be45eb7))
- **crypt** Protect generated snapshots at rest ([#282](https://github.com/noirbizarre/over/issues/282)) - ([cf10a45](https://github.com/noirbizarre/over/commit/cf10a45df1047ff9a43c3517313f60454e5e4e68))
- **exclude** Aggregate linked worktree block contributions - ([4ba571f](https://github.com/noirbizarre/over/commit/4ba571f8572bb37bc2c831c90a458845d88a66f3))
- **exclude** Retain historical checkout leaf protection - ([abefb04](https://github.com/noirbizarre/over/commit/abefb048af03a0e4256a73f2eccae6c12e246308))
- **sync** Preserve generated conflict outcomes in previews - ([7618152](https://github.com/noirbizarre/over/commit/7618152343badd33baaf4c1b181888d3654ecf4b))
- **sync** Protect new checkout leaves with host exclusions - ([cd2fed1](https://github.com/noirbizarre/over/commit/cd2fed153afa871428ca4d92594417fbd7012b45))
- **unapply** Retain unverifiable rendered links with attention - ([e556200](https://github.com/noirbizarre/over/commit/e556200284cc6c4416d5cd22d1ed77016f4e1ba1))
- Align generated checkout safety and lifecycle behavior ([#283](https://github.com/noirbizarre/over/issues/283)) - ([29672d9](https://github.com/noirbizarre/over/commit/29672d9ede2fbfb76ce2f2a871385c6b5b99ceb2))

### 📚 Documentation

- **apply** Describe generated artifact conflict handling - ([f9713ba](https://github.com/noirbizarre/over/commit/f9713baf34ba7781fedbb2d5bed98b458c53a765))
- **crypt** Document manual filters and session automation ([#286](https://github.com/noirbizarre/over/issues/286)) - ([2754d73](https://github.com/noirbizarre/over/commit/2754d73f31d785ba7f20c2acf8512dcb0a3be935))
- **hooks** Clarify context for multiple selected roots - ([71ecce0](https://github.com/noirbizarre/over/commit/71ecce09037de0ae527ec74760645b3bf29a648b))
- **permissions** Clarify policy for generated outputs - ([3721767](https://github.com/noirbizarre/over/commit/37217673b3bc4812d45d99f4f13c0da3ea67a715))
- **restore** Explain generated checkout recovery - ([4e67f85](https://github.com/noirbizarre/over/commit/4e67f85159de2f28b0b70edb885fe77adf2a19c9))
- **rules** Document rendering-only assignments - ([acbe742](https://github.com/noirbizarre/over/commit/acbe7421c131070110a5fbabfec0b962fa5e3f2f))
- **sync** Distinguish declared repository library APIs - ([f1c3018](https://github.com/noirbizarre/over/commit/f1c3018a78ff5df053c051bb907019cb4f3dd15c))

## [0.2.1](https://github.com/noirbizarre/over/compare/0.2.0..0.2.1) - 2026-11-13

### 🐛 Bug Fixes

- **commit** Scope staged commits to requested paths ([#269](https://github.com/noirbizarre/over/issues/269)) - ([916f338](https://github.com/noirbizarre/over/commit/916f338267b952ccf4fdd23b9d28ca250cde4492))
- **status** Keep reconciliation summaries on one line ([#270](https://github.com/noirbizarre/over/issues/270)) - ([0d920a7](https://github.com/noirbizarre/over/commit/0d920a79f7a74c11240260f87f4da20908160875))
- **status** Separate Git changes from reconciliation ([#267](https://github.com/noirbizarre/over/issues/267)) - ([296331e](https://github.com/noirbizarre/over/commit/296331e65fa5971fdd13ece3e145564d5054bc52))

## [0.2.0](https://github.com/noirbizarre/over/compare/0.1.0..0.2.0) - 2026-11-12

### 💫 Features

- **add** Support interactive partial staging - ([7b4f965](https://github.com/noirbizarre/over/commit/7b4f965b6ea4740ae054b7585ca33dc336d29a5b))
- **cli**  🚨 **breaking** Rename --root to --target and support OVER_TARGET ([#232](https://github.com/noirbizarre/over/issues/232)) - ([65adb6b](https://github.com/noirbizarre/over/commit/65adb6b1c67f1fc4b45331a2f3a0ba4e644dc0b0))
- **cli** Expose resolved overlay data as JSON ([#187](https://github.com/noirbizarre/over/issues/187)) - ([413c54c](https://github.com/noirbizarre/over/commit/413c54c8ca2925dd67a4684f2e162d297041bc2d))
- **crypt** Unify repository encryption and native secrets ([#255](https://github.com/noirbizarre/over/issues/255)) - ([95ebe8f](https://github.com/noirbizarre/over/commit/95ebe8f11028bda1dc0a17580b64fd0951161605))
- **devcontainer** Streamline the Over feature and align releases ([#230](https://github.com/noirbizarre/over/issues/230)) - ([85dc557](https://github.com/noirbizarre/over/commit/85dc55734dd9c24695036bcf72e0225a57eb6bff))
- **devcontainer** Streamline the Over feature and align releases - ([30578bd](https://github.com/noirbizarre/over/commit/30578bdb5f245df8e701cef589e69c6541e2e2bf))
- **diff**  🚨 **breaking** Make overlay diffs Git-aware ([#202](https://github.com/noirbizarre/over/issues/202)) - ([35fd2bc](https://github.com/noirbizarre/over/commit/35fd2bc859470049c3959ac9c78246a62c583343))
- **exec** Expose shared machine and paths context ([#218](https://github.com/noirbizarre/over/issues/218)) - ([ffff4a9](https://github.com/noirbizarre/over/commit/ffff4a95314bd5df8fd9ec87bd09f5d3333b8b8a))
- **hooks** Support structured templated lifecycle hooks - ([d133f3a](https://github.com/noirbizarre/over/commit/d133f3a22b7d377cbee52c5936079d92a8a415d4))
- **hooks** Add repository and overlay lifecycle hooks ([#188](https://github.com/noirbizarre/over/issues/188)) - ([a7850a6](https://github.com/noirbizarre/over/commit/a7850a6ced7496edb3a7b97d1cb0ef3088866db5))
- **init**  🚨 **breaking** Bootstrap repositories with a canonical Git remote ([#186](https://github.com/noirbizarre/over/issues/186)) - ([a36cb66](https://github.com/noirbizarre/over/commit/a36cb664eda1a43fe82258966247c54611ad96f6))
- **lint** Validate root overlay declarations ([#236](https://github.com/noirbizarre/over/issues/236)) - ([21d7313](https://github.com/noirbizarre/over/commit/21d731379e19d96681e9ca46028807eec467950b))
- **lint** Group findings by file and add JSON output ([#225](https://github.com/noirbizarre/over/issues/225)) - ([6dbacdd](https://github.com/noirbizarre/over/commit/6dbacdd688a624d497589ab6a774da00abee9fcd))
- **lint** Group diagnostics by overlay and warn transitive consumers ([#221](https://github.com/noirbizarre/over/issues/221)) - ([52cb00a](https://github.com/noirbizarre/over/commit/52cb00a96a3fcaf4abafa891667f75089fa120cb))
- **lint** Detect conflicting target ownership in composed overlays - ([2bc3054](https://github.com/noirbizarre/over/commit/2bc3054dc187ce6dcc2f5d25e832c6a2ae89956a))
- **lint** Support linting a named overlay and its uses ([#189](https://github.com/noirbizarre/over/issues/189)) - ([eed3740](https://github.com/noirbizarre/over/commit/eed374079b8fe0674b8fa9c0e982d0aa0288776f))
- **log** Add styled history and Git revision ranges ([#198](https://github.com/noirbizarre/over/issues/198)) - ([89b5707](https://github.com/noirbizarre/over/commit/89b5707ca918249818fefb1ff1241b4fdf09bcdf))
- **overlays**  🚨 **breaking** Simplify declarations to path strings ([#258](https://github.com/noirbizarre/over/issues/258)) - ([ed614c2](https://github.com/noirbizarre/over/commit/ed614c2f5aa84058cf06a3d4d1ac955e51d86aa3))
- **overlays** Unify ordered cascading path rules ([#254](https://github.com/noirbizarre/over/issues/254)) - ([08847f8](https://github.com/noirbizarre/over/commit/08847f8811753da2f6c8e30cbff194dae83253a0))
- **overlays** Infer overlay selection from context ([#233](https://github.com/noirbizarre/over/issues/233)) - ([9c29c5f](https://github.com/noirbizarre/over/commit/9c29c5f9192cab77283bc30ac2d3ec4ccc3f1609))
- **overlays** Simplify scoped mode and permission rules ([#199](https://github.com/noirbizarre/over/issues/199)) - ([bd2b8b9](https://github.com/noirbizarre/over/commit/bd2b8b92a4c232e1e93b505327e09a7a8f9fcfb8))
- **overlays** Support hierarchical .overignore files ([#181](https://github.com/noirbizarre/over/issues/181)) - ([8461cb4](https://github.com/noirbizarre/over/commit/8461cb4c8d6f23196aac30bca2a1388665b884a4))
- **overlays** Expose resolved data in execution contexts ([#182](https://github.com/noirbizarre/over/issues/182)) - ([9cea06e](https://github.com/noirbizarre/over/commit/9cea06edc131d547cf6542585628028844f62b9f))
- **overlays**  🚨 **breaking** Add directory mappings and checkout staging ([#176](https://github.com/noirbizarre/over/issues/176)) - ([5446810](https://github.com/noirbizarre/over/commit/54468104e3e4456415eae9b50b99bfdeba8b024f))
- **restore** Restore selected virtual-checkout files ([#210](https://github.com/noirbizarre/over/issues/210)) - ([5b9aa41](https://github.com/noirbizarre/over/commit/5b9aa41e395890df60bd37075ab67c4789c0e3c1))
- **schema** Publish root and overlay configuration schemas ([#240](https://github.com/noirbizarre/over/issues/240)) - ([8df7fad](https://github.com/noirbizarre/over/commit/8df7fadcd9390f5c0abc71fc8b89383f27359052))
- **secrets** Add provider-neutral resolution and hook interpolation ([#219](https://github.com/noirbizarre/over/issues/219)) - ([22e0017](https://github.com/noirbizarre/over/commit/22e0017c2055f6e2dbb5f73fee64e9add07a612e))
- **secrets** Add Age encryption and repository-local secret store ([#217](https://github.com/noirbizarre/over/issues/217)) - ([4225406](https://github.com/noirbizarre/over/commit/4225406042664979438b0142fa5d54e1adbe0a5d))
- **status**  🚨 **breaking** Consolidate resolved overlay inspection - ([e51edfe](https://github.com/noirbizarre/over/commit/e51edfe1ce75e269404552901bf7802af2f23b7d))
- **sync** Optionally pull and push the Over source repository ([#237](https://github.com/noirbizarre/over/issues/237)) - ([c874059](https://github.com/noirbizarre/over/commit/c874059a1569870b6c5f2e9055447e19f3e59ace))
- **ui** Add shared spinners and overlay apply progress ([#241](https://github.com/noirbizarre/over/issues/241)) - ([35dd2a2](https://github.com/noirbizarre/over/commit/35dd2a2eca3a12c482ea79cdeb3702b8bb745ab1))

### 🐛 Bug Fixes

- **add** Canonicalize full-file checkout staging - ([23ae17a](https://github.com/noirbizarre/over/commit/23ae17a81d26a1342dd3da9631ae6b1453a6d3e7))
- **add** Reject symlink ancestors before full-file staging - ([2261b67](https://github.com/noirbizarre/over/commit/2261b67ed37098dc694a29f265d525c6dc16b73d))
- **add** Preserve imported symlink referents across sidecar encoding - ([b23bf53](https://github.com/noirbizarre/over/commit/b23bf53cd084928a82258b23f792b3e794c55214))
- **add** Reject ignored symlink ingestion before prompting - ([417d114](https://github.com/noirbizarre/over/commit/417d11465c328ee8cec35b1f675e398d4086d9ce))
- **apply** Preflight composed ownership before side effects - ([5db6551](https://github.com/noirbizarre/over/commit/5db65514d8120ef826d1c08c3de9f8e7a385e723))
- **checkout** Initialize virtual checkouts in existing directories ([#243](https://github.com/noirbizarre/over/issues/243)) - ([57fe5d2](https://github.com/noirbizarre/over/commit/57fe5d25359223caec1a54fb6c16732a7be1adc5))
- **checkout** Recognize raw HEAD content with stale checkpoints - ([aafb072](https://github.com/noirbizarre/over/commit/aafb072441fe040848b904f10bff4dfceb6fcde1))
- **checkout** Share canonical Git snapshots across comparisons - ([60f2b20](https://github.com/noirbizarre/over/commit/60f2b20a8530cee05f5b970fadbe61bb478ae1e6))
- **ci** Ignore generated release files in prek ([#203](https://github.com/noirbizarre/over/issues/203)) - ([8043bc8](https://github.com/noirbizarre/over/commit/8043bc837e5f907af7636a1d01edfa49bb42fd0d))
- **cli** Reject missing overlay selection with no-prompt - ([1435164](https://github.com/noirbizarre/over/commit/1435164d99f7b333aee419620b27c103df68b8ca))
- **commit** Exclude intent-to-add placeholders from staged content - ([3636ca4](https://github.com/noirbizarre/over/commit/3636ca449cb0437237b757ad119c1401e4228e15))
- **commit** Publish filtered source worktree content - ([311923f](https://github.com/noirbizarre/over/commit/311923f7b77bd406fcd01f2947ecce5e2953f6f9))
- **commit** Preserve native source metadata through transactions - ([2770da1](https://github.com/noirbizarre/over/commit/2770da13589d4395d36e5b703316fc70cbf735eb))
- **config** Warn and ignore unknown overlay keys ([#222](https://github.com/noirbizarre/over/issues/222)) - ([aaf09cf](https://github.com/noirbizarre/over/commit/aaf09cf9813534bc3a66c403e80987ed638da8b9))
- **desired** Preserve compatible partial claims with mappings - ([bfbf55f](https://github.com/noirbizarre/over/commit/bfbf55f6e663e0924b5f7136018990877c021464))
- **devcontainer** Make feature installation and lifecycle reliable ([#226](https://github.com/noirbizarre/over/issues/226)) - ([55638e3](https://github.com/noirbizarre/over/commit/55638e3771002143d85bc2e8837c5448f1930106))
- **devcontainer** Align dotfiles home default - ([ea6547c](https://github.com/noirbizarre/over/commit/ea6547cc5b7d53f3fc33461ed3c0a87dd3f78297))
- **diff** Report missing hard-link sources without aborting - ([07ecc51](https://github.com/noirbizarre/over/commit/07ecc51c011f7f2854d7d38de0576d126ab0f240))
- **diff** Recognize hard-link identity in link diagnostics - ([eb7c3a8](https://github.com/noirbizarre/over/commit/eb7c3a88afb3a14b79552de8c11c8a0e9e4c7292))
- **doctor** Group lint diagnostics and check existing exclusions ([#247](https://github.com/noirbizarre/over/issues/247)) - ([395c352](https://github.com/noirbizarre/over/commit/395c352ec8b4fe8fb431183ff7057c4189d6f138))
- **doctor** Inspect state without creating diagnostic locks - ([2995e0e](https://github.com/noirbizarre/over/commit/2995e0e927b0d28be5337888668d584a9f18d425))
- **git** Preserve ignored files during revision checkout - ([4b85dac](https://github.com/noirbizarre/over/commit/4b85dac91816662d00eb4ce094b2433111685529))
- **git** Reconcile explicit fetch refspecs on existing remotes - ([5d3bd5c](https://github.com/noirbizarre/over/commit/5d3bd5c0eb184988450eeab66f3063edd2b73e52))
- **git** Keep add preview overlay selection read-only - ([763ea8f](https://github.com/noirbizarre/over/commit/763ea8f5600e9e2806cae3f3ab58a5210dce2604))
- **install** Preserve per-descriptor manager eligibility - ([9ef07b7](https://github.com/noirbizarre/over/commit/9ef07b7cd50ad3a6bd9da6418454c9549a527ac4))
- **install** Resolve cargo home paths against execution root - ([5ade8f8](https://github.com/noirbizarre/over/commit/5ade8f83ec7e9f980b73771176e9889937107e06))
- **install** Discover managers in dependency-only compositions - ([3583ac9](https://github.com/noirbizarre/over/commit/3583ac95266199a91f31c456a3a87a6ed2dfdda1))
- **install** Honor no-uses during package collection - ([a0d827b](https://github.com/noirbizarre/over/commit/a0d827b8121b0e80556a6c4fb3625da371bc6157))
- **new** Construct prospective overlays for ingestion previews - ([d9935c2](https://github.com/noirbizarre/over/commit/d9935c2b99ec052711c3682ddb3984801ab4b829))
- **overlays** Validate Git logical keys before routing - ([23c1e78](https://github.com/noirbizarre/over/commit/23c1e782a326ac7d8c1eefa4694001aaee1378a7))
- **ownership** Prune ignored checkout source subtrees - ([40f1abb](https://github.com/noirbizarre/over/commit/40f1abb13239f04d86d13b04cf178059e4f597db))
- **ownership** Resolve replaced scaffolds prospectively - ([3c42946](https://github.com/noirbizarre/over/commit/3c42946c8f5658757d0bf4c1c97fe0e8d3059527))
- **partial** Canonicalize optional terminal newline comparisons - ([c01bc1b](https://github.com/noirbizarre/over/commit/c01bc1bec94c5935d5e27300647bfae7a25c0455))
- **paths** Share template namespaces across desired path resolution - ([80b9cde](https://github.com/noirbizarre/over/commit/80b9cde5aff0ef5f828d81cce4a2036fcff2a850))
- **plan** Reject file ancestors consistently on Windows - ([952bed4](https://github.com/noirbizarre/over/commit/952bed4887fe0820cdee038648cf923c1df153bc))
- **plan** Defer children until scaffold replacement is authorized - ([ca302c0](https://github.com/noirbizarre/over/commit/ca302c07b77ecc167e5bb61f502ee1d9a8a7b9a0))
- **restore** Preserve native metadata when detaching macOS hard links - ([f084dc4](https://github.com/noirbizarre/over/commit/f084dc4275d054ddfb149ea28e83bae7b673f525))
- **safety** Strengthen destructive-operation safeguards ([#211](https://github.com/noirbizarre/over/issues/211)) - ([6203e59](https://github.com/noirbizarre/over/commit/6203e595127119a35d548868a8f7d0450da7a3a6))
- **selection** Reserve Git administration boundaries - ([35d53e0](https://github.com/noirbizarre/over/commit/35d53e00e0c0adac83ef16ae0c9e935e2820bf53))
- **status** Report blocked scaffold children without inspecting them - ([01a8367](https://github.com/noirbizarre/over/commit/01a836746a78f19b7c64b25eaf60061e699bac2c))
- **status** Centralize Git diagnostics and summarize doctor health ([#251](https://github.com/noirbizarre/over/issues/251)) - ([6af8c11](https://github.com/noirbizarre/over/commit/6af8c11e98d0ca6c8ce823534c7f0bf491df11d0))
- **status** Limit inspection to overlay files and improve report readability ([#249](https://github.com/noirbizarre/over/issues/249)) - ([de16bbe](https://github.com/noirbizarre/over/commit/de16bbe53436a1ed93df8ee64f5f5db082d17134))
- **sync** Preflight and order virtual checkout shape transitions - ([4af85f6](https://github.com/noirbizarre/over/commit/4af85f65a0ac362c82b0e65fea0d8d1c68ef67e6))
- **sync** Short-circuit abort for virtual checkout units - ([51dc74a](https://github.com/noirbizarre/over/commit/51dc74a626634c8f004719fbae17d43a3ddd80a9))
- **ui** Shorten home paths across all commands ([#263](https://github.com/noirbizarre/over/issues/263)) - ([ff0be47](https://github.com/noirbizarre/over/commit/ff0be47e5bf1936b22e9cbea71bd1d2de1c15070))
- **ui** Simplify reconciliation and hook summaries ([#260](https://github.com/noirbizarre/over/issues/260)) - ([c13cbdd](https://github.com/noirbizarre/over/commit/c13cbdd9c32d33f64242f2bc673d111daa3079fd))
- **ui** Apply semantic colors consistently across CLI output ([#250](https://github.com/noirbizarre/over/issues/250)) - ([fc9df75](https://github.com/noirbizarre/over/commit/fc9df755b291559e52e2185bba593fa6047c9b72))
- **ui** Use theme-colored text glyphs for status indicators ([#242](https://github.com/noirbizarre/over/issues/242)) - ([4e7ab72](https://github.com/noirbizarre/over/commit/4e7ab729e239c45d44f87c1246dbc9fa07873039))
- **unapply** Revalidate checkout content using canonical Git semantics - ([7076ca3](https://github.com/noirbizarre/over/commit/7076ca30746fbdd25177548dbc50b0577bb278aa))
- Reconcile overlay safety and documentation consistency ([#262](https://github.com/noirbizarre/over/issues/262)) - ([2921da8](https://github.com/noirbizarre/over/commit/2921da8cb77aefaf0cf710445788db5d68bd5cb0))
- Align overlay workflows, safety boundaries, and documented behavior ([#213](https://github.com/noirbizarre/over/issues/213)) - ([7fa675f](https://github.com/noirbizarre/over/commit/7fa675ffaaad964dbfe7651d9fe9d6a3a4035804))

### 🔨 Refactor

- **git** Remove unused stateful overlay selection helper - ([94deabf](https://github.com/noirbizarre/over/commit/94deabf275741d5988af704a47108ce31f3a8e02))
- **install**  🚨 **breaking** Replace package installation with lifecycle hooks ([#220](https://github.com/noirbizarre/over/issues/220)) - ([91e1d3e](https://github.com/noirbizarre/over/commit/91e1d3e5aea6ab60808cbca786c665f3b658abf1))
- **json** Standardize CLI output with console-json ([#238](https://github.com/noirbizarre/over/issues/238)) - ([94b86a4](https://github.com/noirbizarre/over/commit/94b86a4612773392e22e8404b9cefb9a50a1a794))

### 📚 Documentation

- **agents** Clarify inward dependency boundaries - ([6540ef7](https://github.com/noirbizarre/over/commit/6540ef7723fb21cd0b81b17f4314e25c8793040d))
- **agents** Describe current diff module responsibilities - ([69854a3](https://github.com/noirbizarre/over/commit/69854a360f1bc5305a0315766d3f85884730fbdb))
- **checkout** Correct lost association recovery guidance - ([87d4af2](https://github.com/noirbizarre/over/commit/87d4af2ca051ac9b20a953435a9b22f23b7d1a67))
- **cli** Qualify crypt and access repository discovery - ([db316ab](https://github.com/noirbizarre/over/commit/db316ab71629349cc1812f0f52d75cb6b1c89eb6))
- **data** Clarify terminal and piped JSON formatting - ([7b0a5fd](https://github.com/noirbizarre/over/commit/7b0a5fd68486a4cfe63499d2b44baac48e538bcc))
- **demo** Refresh the overlay workflow recording ([#239](https://github.com/noirbizarre/over/issues/239)) - ([a2f6490](https://github.com/noirbizarre/over/commit/a2f6490b6c97e52e612232bcc0015269ff3b5802))
- **desired** Correct declared repository synchronization guidance - ([7bbdf6b](https://github.com/noirbizarre/over/commit/7bbdf6bf0a9b07009b0a60e17c0f9b86ada662f5))
- **doctor** Correct named runtime inspection scope - ([2237fa7](https://github.com/noirbizarre/over/commit/2237fa7fa3a31239128ba50f0037389e2deda3a6))
- **faq** Explain Over's origins and compare alternatives ([#256](https://github.com/noirbizarre/over/issues/256)) - ([1e4c11f](https://github.com/noirbizarre/over/commit/1e4c11f84dfb294654a6f14e4583cf76472d17f0))
- **git** Clarify retained plaintext exclusion safeguards - ([3a49ad7](https://github.com/noirbizarre/over/commit/3a49ad76a65ec2caa3172d0ab44456ecea2485ba))
- **install** Remove unsupported registry-based binstall instructions - ([5bce987](https://github.com/noirbizarre/over/commit/5bce9872171066413a676945dfae676f1ce372a1))
- **overlays** Clarify optional local descriptors in getting started - ([844d653](https://github.com/noirbizarre/over/commit/844d653b9e3f17704492a535141283fb430cb840))
- **partial** Correct sidecar duplicate precedence contract - ([47db72e](https://github.com/noirbizarre/over/commit/47db72ec48a99fdc8f5f9a0f127ea2ae2c888f24))
- **plan** Describe preflight before repository provisioning - ([84aa766](https://github.com/noirbizarre/over/commit/84aa7660aee391766fe4590507a6ef8b755103e4))
- **readme** Match supported platforms to release artifacts - ([90ace31](https://github.com/noirbizarre/over/commit/90ace31c29345cfbb7ced128bee5f04e99cefe64))
- **restore** Explain protected content and historical index restrictions - ([37fb04b](https://github.com/noirbizarre/over/commit/37fb04bfed038f2eba214415c7e08bc0e54150d5))
- **secrets** Clarify recoverable native session fallback - ([0336882](https://github.com/noirbizarre/over/commit/03368822217fb3de0cd98cece99bee0be8fff283))
- **secrets** Distinguish native unlock from legacy credentials - ([a8f089d](https://github.com/noirbizarre/over/commit/a8f089d9a5b11316fb4cafcd4bd49afb0e7b4d9a))
- **selection** Remove obsolete overlay chooser examples - ([ea634cd](https://github.com/noirbizarre/over/commit/ea634cdf7bb5434df76fae89adf38df657031a65))
- **selection** Qualify explicit sidecar referent protection - ([0bfcc63](https://github.com/noirbizarre/over/commit/0bfcc63a4f3fe123c2c58f586b5dc3aabcd2289c))
- **selection** Explain reserved Git administration paths - ([a0829c3](https://github.com/noirbizarre/over/commit/a0829c3ae7b442be3ccff7bd54fad3e591d5526e))
- **status** Clarify commit conflict and staging contracts - ([df75f8f](https://github.com/noirbizarre/over/commit/df75f8f2049537ecbb8a9df0f0b2fe8efbc4c28a))
- **status** Explain manual origin URL repair - ([fdb4396](https://github.com/noirbizarre/over/commit/fdb4396576335b18d8ef828d39985da18b620418))
- **sync** Clarify hard-reset abort semantics - ([e8f4869](https://github.com/noirbizarre/over/commit/e8f4869299a631a0a1050b9adae877ec6b2512e4))
- **tooling** Correct Clippy validation guidance - ([42c9e93](https://github.com/noirbizarre/over/commit/42c9e93b2bb573aefce7e92420b7415a22bef9ee))
- **unapply** Describe retained directory reporting - ([f1923eb](https://github.com/noirbizarre/over/commit/f1923eb440b29d56f9dc2aea668cf1f3b843e529))
- **xdg** Clarify unrecoverable checkout association state - ([c8de191](https://github.com/noirbizarre/over/commit/c8de19145e4e3d09779a2c256729980b2676a254))
- L add unified configuration reference and small changes ([#259](https://github.com/noirbizarre/over/issues/259)) - ([1f360d0](https://github.com/noirbizarre/over/commit/1f360d0d7822de93ab81a5b757affee200bf4c90))
- Simplify the getting-started walkthrough ([#224](https://github.com/noirbizarre/over/issues/224)) - ([91625dc](https://github.com/noirbizarre/over/commit/91625dcc20f42a3d53b7bf85b6fcc99208da08b1))
- Restructure user guide and reference documentation ([#207](https://github.com/noirbizarre/over/issues/207)) - ([877794f](https://github.com/noirbizarre/over/commit/877794f78a310c02b80093694ceb06686afaf475))
- Correct virtual checkout link after main sync - ([dbd6f19](https://github.com/noirbizarre/over/commit/dbd6f19de2b57839edfe0a92b807d2b9d88e5039))

### 🧪 Tests

- **checkout** Scope Unix snapshot fixtures to Unix - ([bf2c450](https://github.com/noirbizarre/over/commit/bf2c450acd96f6732051e20adc8967893058cd07))
- **install** Normalize expected descriptor paths on Windows - ([5a2a36b](https://github.com/noirbizarre/over/commit/5a2a36ba27625571b039e25fec48988c793dc115))

### 🏗️ Build

- **deps** Update mise.lock - ([f1beb51](https://github.com/noirbizarre/over/commit/f1beb5168fa8bdcd733c670073c9e5d440aed65a))
- **deps** Bump minijinja ([#177](https://github.com/noirbizarre/over/issues/177)) - ([d32a148](https://github.com/noirbizarre/over/commit/d32a1487fada60df1e672feb0def8fa24de69457))
- **deps** Bump the rust-dependencies group with 4 updates ([#175](https://github.com/noirbizarre/over/issues/175)) - ([4c7d98f](https://github.com/noirbizarre/over/commit/4c7d98f6bfcf302a43b7d3b8439ca376b7d59786))
- **deps** Bump the rust-dependencies group with 2 updates ([#171](https://github.com/noirbizarre/over/issues/171)) - ([382d7bc](https://github.com/noirbizarre/over/commit/382d7bc8cf963b56327001fb908f137ce5edcbda))
- **deps** Bump rstest from 0.27.0 to 0.28.0 ([#172](https://github.com/noirbizarre/over/issues/172)) - ([0315aa2](https://github.com/noirbizarre/over/commit/0315aa2ff5c789adb606877d30d92c88d5ddda44))
- **deps** Bump noyalib from 0.0.57 to 0.0.58 ([#173](https://github.com/noirbizarre/over/issues/173)) - ([0205549](https://github.com/noirbizarre/over/commit/0205549b9d68d8137af32c5480144f80da167162))

### 🔧 CI

- **coverage** Make diagnostic coverage uploads debug-only and best-effort ([#208](https://github.com/noirbizarre/over/issues/208)) - ([c5cb709](https://github.com/noirbizarre/over/commit/c5cb70961e7aea8962ae63e4c78c85388850608d))
- **devcontainer** Rename Dev Container and Docker workflows - ([c2e90ce](https://github.com/noirbizarre/over/commit/c2e90ce6a540d1105681df7693ffccc2e456ac90))
- Distinguish documentation build and publication jobs ([#229](https://github.com/noirbizarre/over/issues/229)) - ([606c278](https://github.com/noirbizarre/over/commit/606c2783983019b9db45aa8404f95719703aad12))
- Restore main CI after overlay feature merges ([#185](https://github.com/noirbizarre/over/issues/185)) - ([4fff023](https://github.com/noirbizarre/over/commit/4fff023cb0977dac12ad0abfc03ece2e48a45bee))

### 🧹 Chores

- **lint** Remove obsolete markdownlint configuration - ([196d020](https://github.com/noirbizarre/over/commit/196d0205d1c3b774afeb3b44354d580c801b1dc9))
- **template** Merge documentation job names update - ([e31078b](https://github.com/noirbizarre/over/commit/e31078b3eb8d4a12cc06ad2c81c629d12471c2bb))
- **template** Merge upstream tools markers and documentation job names - ([886206d](https://github.com/noirbizarre/over/commit/886206d26ef8ffd992b7fe41956d393f5c3b2a64))
- **template** Sync with latest rust.tpl ([#200](https://github.com/noirbizarre/over/issues/200)) - ([109f9f2](https://github.com/noirbizarre/over/commit/109f9f2880981533d3db09413eb7c82e1962f52d))
- **template** Sync with main before submission - ([8e435cf](https://github.com/noirbizarre/over/commit/8e435cf3a1ce14c0fd59b72ec310028a6c831d87))
- **template** Finalize tool lock and documentation checks - ([325f8a4](https://github.com/noirbizarre/over/commit/325f8a49afbf497420e7e7dfd9d3d8395e57c1d4))
- **template** Merge latest rust.tpl updates - ([3f0ddbc](https://github.com/noirbizarre/over/commit/3f0ddbc148a57ee1df79c69793cb7a59c79c7ce6))
- Merge main into documentation job names branch - ([2ab2774](https://github.com/noirbizarre/over/commit/2ab2774f0f05a443157ccbd4d7991c6b30105c0d))

### Tpl

- Render rust at main - ([6a51b16](https://github.com/noirbizarre/over/commit/6a51b16b597ecaededfb153bc5f25a1540d7e11a))
- Render rust at main - ([7fa1a34](https://github.com/noirbizarre/over/commit/7fa1a341e7f70bf70fad70e0372511b28250ab14))
- Render rust at main - ([00aca87](https://github.com/noirbizarre/over/commit/00aca87d00c3fbc43ecc07d8e344653697501bf9))
- Render rust at main - ([854b0ac](https://github.com/noirbizarre/over/commit/854b0ac0f8a75be79fb619efc0df2f07a7e42014))
- Render rust at main - ([86c59e9](https://github.com/noirbizarre/over/commit/86c59e9a5515313d7ad200f0e0bdc24390c50ddb))
- Render rust at main - ([7de2b79](https://github.com/noirbizarre/over/commit/7de2b79902f8d7217162c5292bfc7e70a26c1884))
- Render rust at main - ([f565d94](https://github.com/noirbizarre/over/commit/f565d94fb70e1d29f490709a0591643419c563d1))
- Render rust at main - ([bb180d6](https://github.com/noirbizarre/over/commit/bb180d634fd91e884674a35d5afffbf43bc55a00))

## 0.1.0 - 2026-10-22

### 💫 Features

- **add** Initial `add` command - ([e28b161](https://github.com/noirbizarre/over/commit/e28b1618cf51df888886f0d3dd7ac053d16c8560))
- **apply** Display overlay picker when name is omitted ([#38](https://github.com/noirbizarre/over/issues/38)) - ([9cf3060](https://github.com/noirbizarre/over/commit/9cf30605a6c616aeda2bf4c906b2e53d03d66a1b))
- **apply** Prompt on conflict - ([81f6a3d](https://github.com/noirbizarre/over/commit/81f6a3d1eb6d40062a0a987b1af51cedc2a23bb1))
- **cli** Implement --no-uses flag for apply command - ([8fd21d1](https://github.com/noirbizarre/over/commit/8fd21d19b23f694706197ae7c50a37f78d9643a3))
- **cli** Add shell completion generation command ([#70](https://github.com/noirbizarre/over/issues/70)) - ([d4afef7](https://github.com/noirbizarre/over/commit/d4afef7c5ad61cf3046256f9d09902ed78d0f920))
- **desired** Add file permission metadata resolved via hierarchical rules for partial files ([#136](https://github.com/noirbizarre/over/issues/136)) - ([8e2cf85](https://github.com/noirbizarre/over/commit/8e2cf8550d76d4c5a98665ca7dd9df1f43474094))
- **desired** Add canonical desired filesystem state model ([#116](https://github.com/noirbizarre/over/issues/116)) - ([27c9ab2](https://github.com/noirbizarre/over/commit/27c9ab2fbb9aec66ade406f185d959a80c6779dd))
- **devcontainer** Provide a devcontainer feature to load dotfiles with over - ([c3de3fa](https://github.com/noirbizarre/over/commit/c3de3fa6502b78be506bb7b9d81ba494317fa79c))
- **diff** Add over diff command comparing desired and actual state ([#120](https://github.com/noirbizarre/over/issues/120)) - ([71b3708](https://github.com/noirbizarre/over/commit/71b37080d8d3a16c3620f4a223616ea26679b80d))
- **doctor** Add over doctor to aggregate diagnostics and repair malformed exclude blocks - ([58c276f](https://github.com/noirbizarre/over/commit/58c276f4fc5a46aaa0b13552bca324eed07c3098))
- **git** Remove owned exclude entries on over unapply ([#152](https://github.com/noirbizarre/over/issues/152)) - ([c37dafa](https://github.com/noirbizarre/over/commit/c37dafa84ac543af5aec33ee5a275a905be8d4ff))
- **git** Exclude overlay-managed paths from target repo's git status on apply ([#151](https://github.com/noirbizarre/over/issues/151)) - ([4468abd](https://github.com/noirbizarre/over/commit/4468abd28ad4363d2f98a83311d983647655dbc9))
- **git** Add per-worktree config support - ([7fc33f9](https://github.com/noirbizarre/over/commit/7fc33f9c8f56d78cba7081435f71de8a0fb514fe))
- **git** Advanced git config - ([4bf119d](https://github.com/noirbizarre/over/commit/4bf119dcdc4ac573d6029ffd660b087703b6ace1))
- **git-over** Inital `git-over` implementation - ([2fb3d2c](https://github.com/noirbizarre/over/commit/2fb3d2cd2f008b10433b72fe608d5f126118810a))
- **help** Add colors to help - ([61004e7](https://github.com/noirbizarre/over/commit/61004e72ad383b0f5e78798e5d4f237758cbc3be))
- **install** Parse config - ([ede64a8](https://github.com/noirbizarre/over/commit/ede64a8b527a86a5fbdea0bfba46c93ffad21deb))
- **link** Basic complict and overwriting handling - ([540d20e](https://github.com/noirbizarre/over/commit/540d20e83b6e9ce66320cfdf0bd14c5d47d6def1))
- **lint** Show precise diagnostics with file locations and key suggestions ([#47](https://github.com/noirbizarre/over/issues/47)) - ([957803e](https://github.com/noirbizarre/over/commit/957803ee4963273a201ab1920b342b49fc2e1229))
- **lint** Add a `over lint` command - ([faf4851](https://github.com/noirbizarre/over/commit/faf4851a3234f36c0440d12d210b01b71e84434b))
- **list** Implement --tree view for overlay listing ([#72](https://github.com/noirbizarre/over/issues/72)) - ([0996a8b](https://github.com/noirbizarre/over/commit/0996a8bf64f819681b1d88a07358f67296bb3b63))
- **materialize** Add virtual checkout materialization with commit/log/sync integration ([#141](https://github.com/noirbizarre/over/issues/141)) - ([a20d8e8](https://github.com/noirbizarre/over/commit/a20d8e8c64a2662d851f7a93edf486b4364a77b4))
- **materialize** Migrate legacy symlink-only installations with no prior XDG state ([#135](https://github.com/noirbizarre/over/issues/135)) - ([e5bd02c](https://github.com/noirbizarre/over/commit/e5bd02cf8850f929a0791eca8886dc205174f429))
- **materialize** Introduce Materializer backend registry ([#118](https://github.com/noirbizarre/over/issues/118)) - ([8e5ee41](https://github.com/noirbizarre/over/commit/8e5ee417c78b3c08b8b141fbedeebc4219b6723b))
- **mount** Add support for worktrees - ([7cab6ad](https://github.com/noirbizarre/over/commit/7cab6ad3d93156ac72e5e37419ccbbc56803adf4))
- **new** Initial implementation of the `new` command - ([933343c](https://github.com/noirbizarre/over/commit/933343cf87115eb9d883002c926fc28aeef8df9c))
- **overlay** Add exclude glob patterns for apply and add ([#42](https://github.com/noirbizarre/over/issues/42)) - ([ec501e6](https://github.com/noirbizarre/over/commit/ec501e62824d5ad00c08ec813541e90495d3e17c))
- **overlays** Add default_overlay selection for apply/unapply/status/diff/sync ([#133](https://github.com/noirbizarre/over/issues/133)) - ([b2725e1](https://github.com/noirbizarre/over/commit/b2725e171dbeb6bca81c037d611462d5275bac83))
- **overlays** Root-declared overlays extend descriptor discovery ([#132](https://github.com/noirbizarre/over/issues/132)) - ([bfb8f8a](https://github.com/noirbizarre/over/commit/bfb8f8a8899ba924eff9672b62ff8e032981847b))
- **overlays** Materialization rules with defaults and path/subtree overrides ([#131](https://github.com/noirbizarre/over/issues/131)) - ([f3b2f77](https://github.com/noirbizarre/over/commit/f3b2f771a98e2a10caeb9b54a505ac8bd62ec915))
- **partial** Add partial file management via managed blocks ([#124](https://github.com/noirbizarre/over/issues/124)) - ([8acbf30](https://github.com/noirbizarre/over/commit/8acbf30bcde61d1589d66179e2f503bb51e91032))
- **plan** Reconcile materialization rule changes as explicit migrations ([#134](https://github.com/noirbizarre/over/issues/134)) - ([9c7bbe9](https://github.com/noirbizarre/over/commit/9c7bbe9dbdf005bebff02cef5c45cc4124c3e5e0))
- **plan** Add execution plan and reconciliation pipeline ([#117](https://github.com/noirbizarre/over/issues/117)) - ([46be098](https://github.com/noirbizarre/over/commit/46be0980da2cf23edd0e507a4937cb46e31f75dd))
- **post** String or list for post - ([41612a3](https://github.com/noirbizarre/over/commit/41612a3e9a1e6e5ce74878c9a011bc56c9e9be36))
- **release** Publish AUR and Homebrew packages for over - ([db5363d](https://github.com/noirbizarre/over/commit/db5363d99ff665847617627c46ebc884334fece6))
- **status** Surface git-exclude drift and tracked-path conflicts ([#155](https://github.com/noirbizarre/over/issues/155)) - ([accf654](https://github.com/noirbizarre/over/commit/accf654d68c06d0902e81a6310459b4348a647b5))
- **status** Derive overlay status from the DesiredTree/Plan model ([#119](https://github.com/noirbizarre/over/issues/119)) - ([f2a6782](https://github.com/noirbizarre/over/commit/f2a67820e16c3088dd5316687ca973469582195e))
- **sync** Add bidirectional synchronization for checkout-materialized overlays ([#121](https://github.com/noirbizarre/over/issues/121)) - ([fe5273b](https://github.com/noirbizarre/over/commit/fe5273b0e08227f123c9442d713c5ee0110acf7f))
- **templates** Expose machine info as {{ machine.* }} template variables ([#114](https://github.com/noirbizarre/over/issues/114)) - ([9aac1f4](https://github.com/noirbizarre/over/commit/9aac1f4b282767a0c884fa2ac1b760a822107f4e))
- **unapply** Add over unapply to remove an overlay's own entries ([#123](https://github.com/noirbizarre/over/issues/123)) - ([e96f704](https://github.com/noirbizarre/over/commit/e96f704d79787cb45aa1f71c5c55b7b8bc94687a))
- **utils** Find common suffix - ([7b9a628](https://github.com/noirbizarre/over/commit/7b9a628c874aa6c340eeb896d74f0fb020a7962b))
- **winget** Add `winget` install backend - ([1fabcfa](https://github.com/noirbizarre/over/commit/1fabcfa0cda21b8509e15187c57bf76326e69d79))
- **xdg** Add XDG state/cache layout and persistence boundary ([#115](https://github.com/noirbizarre/over/issues/115)) - ([916948f](https://github.com/noirbizarre/over/commit/916948f0bf7d3bc1749bae97a7f75d27141fd52d))
- Add structured error handling with tracing ([#68](https://github.com/noirbizarre/over/issues/68)) - ([b3c3b54](https://github.com/noirbizarre/over/commit/b3c3b541de9a1dd34c0d0ea71923100dafb1cb9e))
- Add symlink management via .link.(toml|yaml) files ([#41](https://github.com/noirbizarre/over/issues/41)) - ([6f72c2b](https://github.com/noirbizarre/over/commit/6f72c2b305eb127b980add431f64d8ddcaa09972))
- Add and link dir - ([108a27c](https://github.com/noirbizarre/over/commit/108a27ccd987ee1c059e550919941d6d9fcad255))
- Handle link conflict - ([6fc6cd5](https://github.com/noirbizarre/over/commit/6fc6cd5809ecf69c38a5c935d963c489ea5b4e02))
- Nesting - ([6d4a21b](https://github.com/noirbizarre/over/commit/6d4a21bbdd25bd0fda0de6582d1e07bf5bdeb1ed))

### 🐛 Bug Fixes

- **ci** Pin prek to the musl Linux asset in mise.lock - ([c0f5e03](https://github.com/noirbizarre/over/commit/c0f5e032385bdd16bdf5598b2a96a213a6540f4b))
- **ci** Make tests cross-platform compatible for Windows ([#39](https://github.com/noirbizarre/over/issues/39)) - ([a7ee42b](https://github.com/noirbizarre/over/commit/a7ee42b825c26e20623c6971fcd803a384d68e83))
- **cli** Propagate repo.overlays() error in over log instead of swallowing it - ([8a34898](https://github.com/noirbizarre/over/commit/8a34898018e334206ca5023cf9aeb6d89270cf5f))
- **cli** Correct stale git over mount reference to over git mount - ([51e7676](https://github.com/noirbizarre/over/commit/51e76769077037c39c59719fc98d7971a6bf45aa))
- **cli** Route list --tree output through ui:: instead of print! - ([69879b0](https://github.com/noirbizarre/over/commit/69879b08eeca7218e917b5d38979e23ab49f4b43))
- **cli** Make the `home` expected for sub commands only - ([8c75ca6](https://github.com/noirbizarre/over/commit/8c75ca6a76aac8160ee802709e9092e8312815aa))
- **dependencies** Fix wrong circular dependencies resolution on multiple occurrence of transitive dependency - ([9d43762](https://github.com/noirbizarre/over/commit/9d4376262245dd6188adbdd908a53ff4f5fdb7b5))
- **git** Adapt to git2 0.21 API and align git2_credentials - ([8cb9f14](https://github.com/noirbizarre/over/commit/8cb9f1414ae4029f27ddaf8126dd9e31ebd3652d))
- **git** Set `over.overlay` config when cloning repository - ([d11217c](https://github.com/noirbizarre/over/commit/d11217c36a76873348ae81014fc416522c83a51f))
- **git** Support root level git repository in config - ([b30b693](https://github.com/noirbizarre/over/commit/b30b69399cee9ac2a93c6c0fb64e8f34a3a0a3ce))
- **git-exclude** Skip bare enclosing repositories instead of erroring - ([f983198](https://github.com/noirbizarre/over/commit/f983198fbfba2857891e521c4beefe6746214051))
- **lint** Resolve clippy manual-checked-ops and useless-borrows warnings - ([f9beb35](https://github.com/noirbizarre/over/commit/f9beb35e3a90d4b84792218995b6a371c5d0cdbb))
- **lint** Resolve clippy warnings, TOML formatting, and trailing whitespace ([#67](https://github.com/noirbizarre/over/issues/67)) - ([f8f107e](https://github.com/noirbizarre/over/commit/f8f107e8513ddf5ff8a7bd8676bde166b671c58a))
- **materialize** Disable checkout content filters for virtual checkouts - ([3d4b533](https://github.com/noirbizarre/over/commit/3d4b5338e52b26833a91e37d574daff0d3086d2b))
- **new** Ignore target if it is the default target - ([d4a2b6f](https://github.com/noirbizarre/over/commit/d4a2b6f608fa11399a39d6c0c555b3edbbe81935))
- **overlay** Skip badly formatted files with warning ([#40](https://github.com/noirbizarre/over/issues/40)) - ([527d839](https://github.com/noirbizarre/over/commit/527d83957c1ae496de061e5fac6addd1e3394440))
- **overlays** Propagate root config parse errors instead of silencing them - ([d1aa7ec](https://github.com/noirbizarre/over/commit/d1aa7ec9c8af04fff4d813ad9677a16f9ad4460a))
- **status** Treat declared git repositories as provisioned resources ([#140](https://github.com/noirbizarre/over/issues/140)) ([#143](https://github.com/noirbizarre/over/issues/143)) - ([8dcd96a](https://github.com/noirbizarre/over/commit/8dcd96af4e67459bb77306a6df54dde4a5572838))
- **sync** Apply virtual checkout fast-forwards by writing changed blobs directly - ([1757ceb](https://github.com/noirbizarre/over/commit/1757ceb842f8c7d673cc712ee9231760f3bf80b1))
- **test** Make git_over_mount_debug_output robust to platform TTY behavior - ([a766223](https://github.com/noirbizarre/over/commit/a766223c5f1f455074b4c47904835e4702224cd4))
- **windows** Resolve 16 Windows-only test failures - ([4ed7c6d](https://github.com/noirbizarre/over/commit/4ed7c6d725c79cbbfc7cdde371ee586ccda0e6a2))
- Wrap synchronous WalkDir traversals in spawn_blocking - ([5aa9de6](https://github.com/noirbizarre/over/commit/5aa9de633cbd705281ecc5a09ba053706971867e))
- Add missing error context to target-file fs I/O - ([a7cf02d](https://github.com/noirbizarre/over/commit/a7cf02d7138f887239c3129d8705d7b5b8125f05))
- Lots of fixes - ([415fee5](https://github.com/noirbizarre/over/commit/415fee5c1ea88837fb261867830a7c2b11aa75f9))
- Improve output - ([c81ee27](https://github.com/noirbizarre/over/commit/c81ee2713c78956a5c5e3ed2b4f2cb3dc73d1402))
- Improve output - ([539a11e](https://github.com/noirbizarre/over/commit/539a11e72690f0a057d2d09dade0bf360e22425f))
- Improve output - ([9fc92a2](https://github.com/noirbizarre/over/commit/9fc92a299f6eae0fc9f4feb5794f9f9556ef1ca7))

### ⚡ Performance

- **docker** Optimize build time with cargo-chef and native arm64 runners ([#46](https://github.com/noirbizarre/over/issues/46)) - ([c198321](https://github.com/noirbizarre/over/commit/c1983219e69fb921a090b2b2341d1eadc87527f0))

### 🔨 Refactor

- **actions/git** Disambiguate local config module from extern crate - ([44a4617](https://github.com/noirbizarre/over/commit/44a461787f9685297258878800550fccf25b7bd9))
- **apply** Use `root` instead of `target` - ([dd2db23](https://github.com/noirbizarre/over/commit/dd2db234937ab66187dbef8340721d26e2315239))
- **cli** Reuse common::expand_tilde in new, drop avoidable unwrap - ([66ca375](https://github.com/noirbizarre/over/commit/66ca37571d85a34cb85162692e10b68e5cb5bb59))
- **cli** Merge git-over into over as a git subcommand group ([#150](https://github.com/noirbizarre/over/issues/150)) - ([195f9a5](https://github.com/noirbizarre/over/commit/195f9a580b1fbec3294ff8cf06e54ee06ce287f3))
- **cli** Migrate println! debug output to tracing::debug! - ([7cd1de6](https://github.com/noirbizarre/over/commit/7cd1de688c847aa5bb4f3948f4171853ef2a60a3))
- **status,diff** Unify Report predicate naming to needs_attention - ([2b7b7da](https://github.com/noirbizarre/over/commit/2b7b7dafe97c11c2488f8d90cef2ce72dde06c96))
- **templates** Centralize MiniJinja template environment ([#71](https://github.com/noirbizarre/over/issues/71)) - ([b17a2e4](https://github.com/noirbizarre/over/commit/b17a2e4f42bf69f53fcb3bc70d72bd2f50ff4608))
- **templates** Use `minijinja` instead of `tera` - ([af595d0](https://github.com/noirbizarre/over/commit/af595d04530768c44aa44c3d0b9fe5b2b9add62c))
- **ui** Route all user-facing output through ui::info/ui::warn - ([3058865](https://github.com/noirbizarre/over/commit/3058865ab7dd9c12da6833f07bb21e467509bf43))
- **yaml** Migrate from serde_yml to noyalib - ([6d9857a](https://github.com/noirbizarre/over/commit/6d9857a40358981649f766e3ed85a3282b30bce6))
- Resolve dead commented-out code - ([4290b46](https://github.com/noirbizarre/over/commit/4290b46bb662d2d806dc857faf59ce3dd59746ee))
- Fix code consistency, clippy warnings, and documentation ([#54](https://github.com/noirbizarre/over/issues/54)) - ([432ef96](https://github.com/noirbizarre/over/commit/432ef965883d201f97458991881bb0c48e054504))
- Big cleanup - ([984161e](https://github.com/noirbizarre/over/commit/984161e9503cb826991c73eec0f60874f488bbab))

### 📚 Documentation

- **AGENTS** Add AGENTS.md - ([911f064](https://github.com/noirbizarre/over/commit/911f064411de1c6236f2d800da4cb28d0c46883f))
- **adr** Clarify ADR-002's no-built-in-history claim isn't stale - ([a3f5c06](https://github.com/noirbizarre/over/commit/a3f5c06fd185dd46f56cbc84dfbe3b23cdf0779b))
- **agents** Update materialize/ module description to list all backends - ([a04c8a1](https://github.com/noirbizarre/over/commit/a04c8a1b3c6ac9da4e3411eedb4858ed8e66b51f))
- **agents** Fix stale src layout table (commit/doctor/git_exclude/utils.rs) - ([fe95a9e](https://github.com/noirbizarre/over/commit/fe95a9e494fc2f32c3cf6b991c4ce87c4944be0b))
- **agents** Add missing modules to Layout tree - ([dfcfb7b](https://github.com/noirbizarre/over/commit/dfcfb7bdc442758cebf05ae7aa8b1191708dec22))
- **agents** Expand prek hooks description to include all checks - ([54836ab](https://github.com/noirbizarre/over/commit/54836ab0cc75220664638423e9b6c49ce8377f58))
- **agents** Remove reference to nonexistent Expect type alias - ([2e8caa6](https://github.com/noirbizarre/over/commit/2e8caa67b101a288ca98dde641ea46e3380b498b))
- **cargo** Fix grammar in package description - ([75f4ec7](https://github.com/noirbizarre/over/commit/75f4ec75979089ed1cf6a068ecdcadba760f2b8a))
- **configuration** Correct Partial Files target templating description - ([d9c3170](https://github.com/noirbizarre/over/commit/d9c317021f168c51d670e809c4c26e65806ae5b6))
- **configuration** Document the checkout materialization value - ([b57c70f](https://github.com/noirbizarre/over/commit/b57c70f389d56303fe8b4c718d5ecbe16241509d))
- **configuration** Add over commit to default overlay selection list - ([e99e6f9](https://github.com/noirbizarre/over/commit/e99e6f91f7d1ab182a0d2dd72c944ce75e7c73f3))
- **desired** Fix stale EntryKind reachability claim - ([168fd42](https://github.com/noirbizarre/over/commit/168fd425e787bca7e30c51038870bff808487347))
- **doctor** Explain why default_overlay/--all don't apply to over doctor - ([95adf35](https://github.com/noirbizarre/over/commit/95adf35be4fe4d16a2302839e12ba735ceca94bf))
- **exec** Document the Ctx/&Ctx/&Context parameter convention - ([d17dbdc](https://github.com/noirbizarre/over/commit/d17dbdc4edfd20e8ae3bcb0fc137b736a66512fd))
- **install-config** Fix broken archlinux Platform Sections example - ([e2e4c01](https://github.com/noirbizarre/over/commit/e2e4c01abb2e47443603646dc6e54c6567da4c2f))
- **logo** Add an inital logo ([#44](https://github.com/noirbizarre/over/issues/44)) - ([c75d38e](https://github.com/noirbizarre/over/commit/c75d38e849e87b5d5e8d2fe82881410231269eab))
- **overlays** Fix Repository::get doc comment terminology - ([47547d7](https://github.com/noirbizarre/over/commit/47547d725ef4b3dacbd92f8b93fc33f16623f3f3))
- **plan** Align Operation::Deferred doc with its own Display impl - ([15bb102](https://github.com/noirbizarre/over/commit/15bb102b30b61b94244c8bc36f9d41070bf24055))
- **readme** List over diff, sync, and unapply commands - ([a9a426e](https://github.com/noirbizarre/over/commit/a9a426e0029218b22c607b4ff3abfc5225bdef5d))
- **readme** Change logo size ([#104](https://github.com/noirbizarre/over/issues/104)) - ([354b0d0](https://github.com/noirbizarre/over/commit/354b0d0b3d17fa1bb77ac116964804cba328f577))
- **readme** Remove outdated TBD note - ([0e19dd4](https://github.com/noirbizarre/over/commit/0e19dd49264ca99d869b3bb8199f879d5777898f))
- **readme** Clarify install execution order and add Windows manager - ([68267b6](https://github.com/noirbizarre/over/commit/68267b6f731d044c4a911e04d8eac7084ee6d367))
- **readme** Standardize project identity as overlay manager - ([05ee5d2](https://github.com/noirbizarre/over/commit/05ee5d2bf4b9ef1a594c2b17ccf459d8c1476df3))
- **readme** Add command reference table for all CLI commands - ([03db186](https://github.com/noirbizarre/over/commit/03db186d43aa4871b4e36df3b1a41f6b81e9a22c))
- **readme** Add winget to supported managers list and examples - ([e524363](https://github.com/noirbizarre/over/commit/e52436394c92336d817bd31c4df4b06f27d8bb9e))
- **readme** Update Windows section to reflect actual winget support - ([524333f](https://github.com/noirbizarre/over/commit/524333fc2b472d54ea1fe469dd83ffca4f16c327))
- **usage** Document the over status taxonomy - ([cade0d2](https://github.com/noirbizarre/over/commit/cade0d28ad9e195bc7f7b719a91cf75dab8f01bf))
- **usage** Mention the structured plan preview in over apply - ([5f7f3e6](https://github.com/noirbizarre/over/commit/5f7f3e6ecca88b7104c7b5e129cbc347e6be2b4e))
- **usage** Fix over apply's described execution order - ([4de4aff](https://github.com/noirbizarre/over/commit/4de4affa6802d2a9be92d14972fc38cdf0a84afd))
- **usage** Document over status full CLI synopsis - ([58eab28](https://github.com/noirbizarre/over/commit/58eab28507c03eb724a5112bcebbca2ad74ce459))
- Add installation, quickstart, layout examples, and a demo video ([#162](https://github.com/noirbizarre/over/issues/162)) - ([0690f58](https://github.com/noirbizarre/over/commit/0690f587ccd4b280df875948b1962751f9424cf7))
- Fix minor doc gaps (machine.* context, backtick, nextest/lib.rs wording) - ([2475573](https://github.com/noirbizarre/over/commit/247557309f0cb627c37677667787b29ce2631f2a))
- Fix stale git over mount reference in ADR-002 - ([ba6e27b](https://github.com/noirbizarre/over/commit/ba6e27b30f4d1b8dbe277895e38b684f4e8dc83a))
- Add missing over doctor/commit/log to README commands table - ([9bb9ced](https://github.com/noirbizarre/over/commit/9bb9ced3e6ec54a0619534facca2a50b2d3a7437))
- Sweep stale #110-pending rustdoc comments - ([f3b61dd](https://github.com/noirbizarre/over/commit/f3b61dd74f9f5ffab025402d8e74979d13d7ee70))
- Add usage/configuration reference pages and ten ADRs - ([5a0f913](https://github.com/noirbizarre/over/commit/5a0f9131c28246cbcced15a29d8620d98e2babd3))

### 🧪 Tests

- **cli** Disable commit.gpgsign in git fixture commits ([#159](https://github.com/noirbizarre/over/issues/159)) - ([bd99c74](https://github.com/noirbizarre/over/commit/bd99c749bef7a5ed708bc2392470e8a7500f11e9))
- **cli** Collapse near-duplicate debug_output tests into rstest cases - ([6a3e785](https://github.com/noirbizarre/over/commit/6a3e7853c37feb57ae78acce7714fea463377d66))
- **cli** Improve patch coverage for tracing debug output and no-uses - ([200840f](https://github.com/noirbizarre/over/commit/200840f4a64f64e408c0aa342500d708b15c8796))
- **exec** Adopt rstest for Context::builder flag-combination tests - ([de81068](https://github.com/noirbizarre/over/commit/de81068f3894a2e5102e325a4410714714d52018))
- **git** Cover git2 0.21 Result-based branches touched by the API fix - ([c2e88aa](https://github.com/noirbizarre/over/commit/c2e88aa1ca3f2510101dcba8416f6eea2024a4d2))
- Rename test_* functions to sentence-style names (batch 3/3) - ([2df3e9e](https://github.com/noirbizarre/over/commit/2df3e9ea0c0262a8000be00e060131ebca94d25e))
- Rename test_* functions to sentence-style names (batch 2/3) - ([8904593](https://github.com/noirbizarre/over/commit/8904593a65915ec61b435f184fe45276b83e015d))
- Rename test_* functions to sentence-style names (batch 1/3) - ([40d969d](https://github.com/noirbizarre/over/commit/40d969daaeff03e1c51efd9cd34a65c79c426269))
- Isolate $XDG_STATE_HOME across sync/virtual-checkout tests - ([8f8ae2e](https://github.com/noirbizarre/over/commit/8f8ae2e627f63ba15acd88c0823ceb18d52a57e2))
- Cover virtual checkout code paths left untested by the initial patch - ([f8a1e00](https://github.com/noirbizarre/over/commit/f8a1e005581e07943b9f4d8c0ba0ea860e9a183d))
- Raise test coverage above 90% ([#45](https://github.com/noirbizarre/over/issues/45)) - ([5216fd4](https://github.com/noirbizarre/over/commit/5216fd49272b3057d7a7639757b9b1ac4cd53613))
- Add unit tests to improve coverage from 75% to 79% ([#43](https://github.com/noirbizarre/over/issues/43)) - ([70b1937](https://github.com/noirbizarre/over/commit/70b19378c7cf65f582ec80611c19f21b000ed4bc))
- Raise coverage - ([ccd8c92](https://github.com/noirbizarre/over/commit/ccd8c9204b0ba5ba9d3446d592545c5632acdb96))
- More tests - ([581f4cd](https://github.com/noirbizarre/over/commit/581f4cd1c4dc058ea5c818f885037fc89fdee478))
- Add missing test file - ([5927089](https://github.com/noirbizarre/over/commit/5927089ca087122f809e99b641532a11d0f66789))

### 🎨 Style

- **actions** Rename MoveFile/copy_dir_recursive params to source/target - ([6b62ad3](https://github.com/noirbizarre/over/commit/6b62ad39eff5da2c026792ad8d3119129bf37eed))
- **cli** Drop redundant Repository alias in unapply test module - ([ea44988](https://github.com/noirbizarre/over/commit/ea4498883112839469cee86ba5414546dfef14dc))
- **cli** Use imported bail! instead of fully-qualified anyhow::bail! - ([2a66eac](https://github.com/noirbizarre/over/commit/2a66eacfb85b931f93fcf7e10d34dcf728b806ca))
- **cli** Drop redundant doc comments in new.rs Params, use help= consistently - ([d448350](https://github.com/noirbizarre/over/commit/d44835067d27f7d3c89aff8e560c4da7468574b3))
- **lint** Unify summary styling with crate::ui::style, matching doctor - ([d530419](https://github.com/noirbizarre/over/commit/d5304194c735d8a6c1a83d33f7ab14539e168c01))
- **lint** Replace guarded unwrap() with is_none_or in suggest_key - ([374a9fb](https://github.com/noirbizarre/over/commit/374a9fb2a3d67241b2deff8c333c5a98b6e12032))
- Lowercase error messages to match the anyhow convention - ([46b0224](https://github.com/noirbizarre/over/commit/46b0224068c0c6ceb48caf408ae3bbeb312a87da))
- Normalize crate:: import grouping to one path per line - ([f310f40](https://github.com/noirbizarre/over/commit/f310f40c00eb2d6bdf96eca92227972a32e0d19b))
- Standardize on .ok() over let _ = for discarded ui:: writes - ([f7b8b67](https://github.com/noirbizarre/over/commit/f7b8b67b6c676fb509d3911ff99e46f1623c52dd))
- Prefer bail! over return Err(anyhow!()) for guard clauses - ([1ff7a8f](https://github.com/noirbizarre/over/commit/1ff7a8ff6bf42b830e588880f28d9550ab41a3f5))
- Standardize on anyhow::Context as _ import alias - ([3029cb2](https://github.com/noirbizarre/over/commit/3029cb28a2b759dacd029bb934458f75855f489d))
- Standardize anyhow::Context aliasing to Context as _ - ([725bf10](https://github.com/noirbizarre/over/commit/725bf105f67bb9b951155bf73a9c3284053c4e1d))
- Group std::sync::LazyLock with the rest of the std imports - ([e75d690](https://github.com/noirbizarre/over/commit/e75d690156e7a6ec393f5ad689c30ff84c71052d))
- Fix import grouping order (std, external, crate) - ([c7c962d](https://github.com/noirbizarre/over/commit/c7c962d8a5d7f0ef8e787ddecb26c9c4ceac3145))
- Format and lint - ([9c0a349](https://github.com/noirbizarre/over/commit/9c0a349eb34a9595dd1142ca354fd89ebda5be32))

### 🏗️ Build

- **deps** Bump minijinja from 2.24.0 to 3.0.0 ([#170](https://github.com/noirbizarre/over/issues/170)) - ([0939f28](https://github.com/noirbizarre/over/commit/0939f2843b7eeb6c5dc5578eb39d121e49b90973))
- **deps** Bump the rust-dependencies group with 2 updates ([#168](https://github.com/noirbizarre/over/issues/168)) - ([a3d194f](https://github.com/noirbizarre/over/commit/a3d194f91536bfa787dbc89df0d5484bab8b53ff))
- **deps** Bump noyalib from 0.0.54 to 0.0.57 ([#169](https://github.com/noirbizarre/over/issues/169)) - ([7fcda8c](https://github.com/noirbizarre/over/commit/7fcda8c0ee60733ce21fe5363eecba1dbfe1c474))
- **deps** Bump jdx/mise-action from 4 to 5 in the actions group ([#167](https://github.com/noirbizarre/over/issues/167)) - ([169ec28](https://github.com/noirbizarre/over/commit/169ec287bb79b61867ff76ae724a9a9be45b2407))
- **deps** Bump the rust-dependencies group with 2 updates ([#165](https://github.com/noirbizarre/over/issues/165)) - ([8c1e0c9](https://github.com/noirbizarre/over/commit/8c1e0c99f2721ea11d24034d3b3ec836dc43cd2e))
- **deps** Bump noyalib from 0.0.51 to 0.0.54 ([#166](https://github.com/noirbizarre/over/issues/166)) - ([c04aeb6](https://github.com/noirbizarre/over/commit/c04aeb616a5b062ce475b5ff26ce16de85019fae))
- **deps** Bump config from 0.15.25 to 0.15.26 in the rust-dependencies group ([#163](https://github.com/noirbizarre/over/issues/163)) - ([b3160f6](https://github.com/noirbizarre/over/commit/b3160f6e4bf14f13da8a85423e3275e3ccc7061d))
- **deps** Bump noyalib from 0.0.44 to 0.0.51 ([#164](https://github.com/noirbizarre/over/issues/164)) - ([cc63c2c](https://github.com/noirbizarre/over/commit/cc63c2c45ac887dc41b96b4bc30a7e15f44b694e))
- **deps** Bump the rust-dependencies group with 3 updates ([#153](https://github.com/noirbizarre/over/issues/153)) - ([fc3a83f](https://github.com/noirbizarre/over/commit/fc3a83f1ff5f019c7a89ad3ace8c7c890108ac6b))
- **deps** Bump noyalib from 0.0.43 to 0.0.44 ([#154](https://github.com/noirbizarre/over/issues/154)) - ([2bb2d55](https://github.com/noirbizarre/over/commit/2bb2d559cb0856597bbcd264c9012f9ba2f9f10f))
- **deps** Bump dirs from 6.0.0 to 7.0.0 ([#137](https://github.com/noirbizarre/over/issues/137)) - ([12ab765](https://github.com/noirbizarre/over/commit/12ab7654a1df05eee8e785aa1e688061e08bb1f8))
- **deps** Bump noyalib from 0.0.30 to 0.0.43 ([#138](https://github.com/noirbizarre/over/issues/138)) - ([0c9507e](https://github.com/noirbizarre/over/commit/0c9507e1d45e9ef8042daab79f889c2ada1c8eeb))
- **deps** Bump rstest from 0.26.1 to 0.27.0 ([#139](https://github.com/noirbizarre/over/issues/139)) - ([ff2e6e2](https://github.com/noirbizarre/over/commit/ff2e6e2b9918ce1b55c34a0491d478a167f9a25c))
- **deps** Bump noyalib from 0.0.28 to 0.0.30 ([#122](https://github.com/noirbizarre/over/issues/122)) - ([b1fd043](https://github.com/noirbizarre/over/commit/b1fd043b54f47c7b28e16cb3c9f066e0018c1832))
- **deps** Bump which from 8.0.5 to 8.0.6 in the rust-dependencies group ([#106](https://github.com/noirbizarre/over/issues/106)) - ([dc20c7e](https://github.com/noirbizarre/over/commit/dc20c7e64a73cfc477dc3e9f7a53492adb273334))
- **deps** Bump the rust-dependencies group with 9 updates ([#101](https://github.com/noirbizarre/over/issues/101)) - ([3c945a5](https://github.com/noirbizarre/over/commit/3c945a59e24a4a7ed38042274c65df7770ed8dcd))
- **deps** Bump noyalib from 0.0.24 to 0.0.28 ([#102](https://github.com/noirbizarre/over/issues/102)) - ([a7e7fdb](https://github.com/noirbizarre/over/commit/a7e7fdb269f9b916e9965b315d08de97fc605e69))
- **deps** Bump git2 from 0.20.4 to 0.21.0 - ([a702283](https://github.com/noirbizarre/over/commit/a70228303e803ef4e94a50624655c8e8ceb5e53d))
- **deps** Bump minijinja from 2.20.0 to 2.24.0 ([#96](https://github.com/noirbizarre/over/issues/96)) - ([5a15abf](https://github.com/noirbizarre/over/commit/5a15abf36c7c9fbcb5011c71638439dd25f34298))
- **deps** Bump futures from 0.3.32 to 0.3.34 ([#97](https://github.com/noirbizarre/over/issues/97)) - ([f1a71a8](https://github.com/noirbizarre/over/commit/f1a71a89450e6b5ace882b7130551bde1188d8d9))
- **deps** Bump anyhow from 1.0.102 to 1.0.104 ([#94](https://github.com/noirbizarre/over/issues/94)) - ([a357718](https://github.com/noirbizarre/over/commit/a357718f9ba9607fe7a2a4878e25d51c29e39065))
- **deps** Bump docker/setup-buildx-action from 3 to 4 in the actions group ([#99](https://github.com/noirbizarre/over/issues/99)) - ([3b1f681](https://github.com/noirbizarre/over/commit/3b1f681cada7b0467c3fbe67603a873bf24fe2e2))
- **deps** Bump the actions group with 6 updates ([#91](https://github.com/noirbizarre/over/issues/91)) - ([29e3196](https://github.com/noirbizarre/over/commit/29e31965c8eebc47496469de5df9872e5f72730b))
- **deps** Bump libz-sys from 1.1.28 to 1.1.29 ([#90](https://github.com/noirbizarre/over/issues/90)) - ([672fe3d](https://github.com/noirbizarre/over/commit/672fe3d7d14be71ae8061dfd9a0f14f96b811ed7))
- **deps** Bump console from 0.16.3 to 0.16.4 ([#88](https://github.com/noirbizarre/over/issues/88)) - ([a461d40](https://github.com/noirbizarre/over/commit/a461d408f6f49425e19ac21a1709014bc35a23ac))
- **deps** Bump globset from 0.4.18 to 0.4.20 ([#86](https://github.com/noirbizarre/over/issues/86)) - ([ac74517](https://github.com/noirbizarre/over/commit/ac74517ffbb6ff015a193fb5ef4a407bc2d08536))
- **deps** Bump async-trait from 0.1.89 to 0.1.92 ([#87](https://github.com/noirbizarre/over/issues/87)) - ([5e7f836](https://github.com/noirbizarre/over/commit/5e7f836da9d4a07ca43bc8026653f167beead6fe))
- **deps** Bump minijinja from 2.19.0 to 2.20.0 ([#82](https://github.com/noirbizarre/over/issues/82)) - ([f392d57](https://github.com/noirbizarre/over/commit/f392d574f44c9aa2ab218b3801a3398ef6e93e83))
- **deps** Bump tokio from 1.51.1 to 1.52.3 ([#79](https://github.com/noirbizarre/over/issues/79)) - ([70382e1](https://github.com/noirbizarre/over/commit/70382e12c6941bdd4a445f640e65d2daf257473e))
- **deps** Bump assert_cmd from 2.2.0 to 2.2.2 ([#81](https://github.com/noirbizarre/over/issues/81)) - ([6f0af24](https://github.com/noirbizarre/over/commit/6f0af24d461227a97155d4f5ed8e9e40ffe2e38e))
- **deps** Bump clap_complete from 4.6.1 to 4.6.5 ([#80](https://github.com/noirbizarre/over/issues/80)) - ([aebe9b1](https://github.com/noirbizarre/over/commit/aebe9b15c827a8d94e6b86e8b0913f8d52c6a05a))
- **deps** Bump clap from 4.6.0 to 4.6.1 ([#73](https://github.com/noirbizarre/over/issues/73)) - ([bdbf512](https://github.com/noirbizarre/over/commit/bdbf512b3b7824d6148a478571d39eb112aa2f20))
- **deps** Bump tokio from 1.50.0 to 1.51.0 ([#52](https://github.com/noirbizarre/over/issues/52)) - ([3979c1a](https://github.com/noirbizarre/over/commit/3979c1a988c248df9429f5f3887594c14ea342eb))
- **deps** Bump minijinja from 2.18.0 to 2.19.0 ([#49](https://github.com/noirbizarre/over/issues/49)) - ([9612eb4](https://github.com/noirbizarre/over/commit/9612eb42da38b69817c2e70719e5e86f1b734b21))
- **deps** Bump rstest from 0.25.0 to 0.26.1 ([#50](https://github.com/noirbizarre/over/issues/50)) - ([8625c3e](https://github.com/noirbizarre/over/commit/8625c3e0c10efcaf5e2e91faaa7938e1dbc9cee6))
- **deps** Bump config from 0.15.21 to 0.15.22 ([#51](https://github.com/noirbizarre/over/issues/51)) - ([0ae1275](https://github.com/noirbizarre/over/commit/0ae127593e2e5cfbd8fb579a4e5eebf0db26a4d4))
- **deps** Bump libz-sys from 1.1.25 to 1.1.28 ([#53](https://github.com/noirbizarre/over/issues/53)) - ([3424a6e](https://github.com/noirbizarre/over/commit/3424a6e61158b2dab08e9a3139501b9bc2a8e2da))
- **deps** Update all dependencies - ([806a144](https://github.com/noirbizarre/over/commit/806a144f837f206c60c456dd49ae4fb8eef50164))
- **deps** Bump async-trait from 0.1.57 to 0.1.88 ([#24](https://github.com/noirbizarre/over/issues/24)) - ([d1efd45](https://github.com/noirbizarre/over/commit/d1efd456054c2dfcf8a363c8dc751d039e944512))
- **deps** Bump which from 7.0.2 to 7.0.3 ([#25](https://github.com/noirbizarre/over/issues/25)) - ([e57a08f](https://github.com/noirbizarre/over/commit/e57a08f4aa811f1b35e0615686a97ea717d8c2c1))
- **deps** Bump config from 0.15.8 to 0.15.11 ([#26](https://github.com/noirbizarre/over/issues/26)) - ([9f2e2be](https://github.com/noirbizarre/over/commit/9f2e2beed66b67147d71b21ae2045c8f066c8372))
- **deps** Bump futures from 0.3.24 to 0.3.31 ([#27](https://github.com/noirbizarre/over/issues/27)) - ([596b571](https://github.com/noirbizarre/over/commit/596b57146a8f07863994e6eae7d5a843d02b33c2))
- **deps** Bump tokio from 1.43.0 to 1.44.2 ([#28](https://github.com/noirbizarre/over/issues/28)) - ([a28387b](https://github.com/noirbizarre/over/commit/a28387b0602c05e627387bdf2a743e8b8f526828))
- **deps** Bump clap from 4.5.30 to 4.5.37 ([#19](https://github.com/noirbizarre/over/issues/19)) - ([a2b9870](https://github.com/noirbizarre/over/commit/a2b98708c2e0003ce3a49deb403086262a4cf227))
- **deps** Bump anyhow from 1.0.64 to 1.0.98 ([#20](https://github.com/noirbizarre/over/issues/20)) - ([19ae10e](https://github.com/noirbizarre/over/commit/19ae10e35bb3de7ea0c4f9430a7f7557e82f251e))
- **deps** Bump globset from 0.4.9 to 0.4.13 ([#21](https://github.com/noirbizarre/over/issues/21)) - ([41945ab](https://github.com/noirbizarre/over/commit/41945abdb9c3579864a70b5b3a08e010b88a2080))
- **deps** Bump thiserror from 2.0.11 to 2.0.12 ([#22](https://github.com/noirbizarre/over/issues/22)) - ([06531dd](https://github.com/noirbizarre/over/commit/06531dd458a0409ce6d038b9edd0ba85da5a224d))
- **deps** Bump console from 0.15.1 to 0.15.11 ([#23](https://github.com/noirbizarre/over/issues/23)) - ([fec9e35](https://github.com/noirbizarre/over/commit/fec9e357c0af2e2b08fc110f021502f7a65b2d9f))
- **install** Add just install task - ([524eca3](https://github.com/noirbizarre/over/commit/524eca37436bb412e7617859cd6b8e40afaaeea0))
- **just** Install tools and added coverage - ([ba00be6](https://github.com/noirbizarre/over/commit/ba00be60e99a700dcfc2ee894de43c1583fc5856))
- **release** Fix for publication - ([1557cc7](https://github.com/noirbizarre/over/commit/1557cc75fc414611333c7f247fce4105dbd82c1e))
- Use `mise` instead of `just` as build tool - ([e2c1cf9](https://github.com/noirbizarre/over/commit/e2c1cf96f3e80610f45e5a9e91cf393aa5c2aedb))
- Basic justfile - ([ecd50f6](https://github.com/noirbizarre/over/commit/ecd50f6bcf30f35d526892720a034f0830842a5f))

### 🔧 CI

- **codecov** Remove old config file - ([ccb7ff2](https://github.com/noirbizarre/over/commit/ccb7ff2f2cda416ec2141948ab4938f159647c39))
- **codecov** Tune codecov config ([#84](https://github.com/noirbizarre/over/issues/84)) - ([e1be00e](https://github.com/noirbizarre/over/commit/e1be00ed143e262c8686d907a8ff7ad9634b6805))
- **dependabot** Update actions using dependabot ([#85](https://github.com/noirbizarre/over/issues/85)) - ([3eb762e](https://github.com/noirbizarre/over/commit/3eb762e244ba6934d7b1307727747f5135831b14))
- **docs** Fix broken docs build and gate deployment to PRs vs main ([#105](https://github.com/noirbizarre/over/issues/105)) - ([efbf1e2](https://github.com/noirbizarre/over/commit/efbf1e221d2f50b44c1f44eacafcffae9723b5ee))
- **junit** Add junit tests upload to codecov ([#69](https://github.com/noirbizarre/over/issues/69)) - ([d2208c0](https://github.com/noirbizarre/over/commit/d2208c007b85ea5c16b4000895d0a672aa0cf804))
- **pre-commit** Use `prek` instead of `pre-commit` - ([9c77e4b](https://github.com/noirbizarre/over/commit/9c77e4b3636e3e0e481a747a3d80ae040ffb7ccc))
- **pre-commit** Add `cargo fmt` and `cargo clippy` checks - ([79348b5](https://github.com/noirbizarre/over/commit/79348b5458c321c0e892ddca2856f287b859b692))
- **pre-commit** Add initial pre-commit config - ([7a0260c](https://github.com/noirbizarre/over/commit/7a0260c77e529876ad080538a487eff8474d952b))
- **test** Add initial CI testing ([#7](https://github.com/noirbizarre/over/issues/7)) - ([53d706a](https://github.com/noirbizarre/over/commit/53d706af84ec60e49ad409dba976d9dbb1e897fd))
- **windows** Fail the CI job when tests fail on Windows - ([9eed298](https://github.com/noirbizarre/over/commit/9eed2982cf5d1afb9a213ea44ff8e6bc76c00f97))
- Regenerate mise.lock with cross-platform entries for the template's new tools - ([601afc0](https://github.com/noirbizarre/over/commit/601afc08b56416a56c08960355dd95006e34e935))

### 🧹 Chores

- **actions** Remove unreferenced systemd.rs, document templates.rs scaffolding - ([141d132](https://github.com/noirbizarre/over/commit/141d132e0f512b6c2259388caae6a99716816bb6))
- **docs** Remove the unused Zola site skeleton in favour of Zensical - ([98244bf](https://github.com/noirbizarre/over/commit/98244bf1d6b8fb16195602ad9635a302fbdd648e))
- **git** Make Cargo.lock binary - ([92c4bb2](https://github.com/noirbizarre/over/commit/92c4bb27e2caa4283d80ea5f21ba7d2a17ff0b2f))
- **style** Use console style instead of owo - ([0a41739](https://github.com/noirbizarre/over/commit/0a417399a8ea7e54b4d4c3d26daf2e4692b7d7a0))
- **template** Drop crates.io publishing and sync rust.tpl ([#161](https://github.com/noirbizarre/over/issues/161)) - ([fefd5fa](https://github.com/noirbizarre/over/commit/fefd5fa2dce9a08ef69081459bb3431e40cba4a1))
- **template** Attach noirbizarre/rust.tpl and resolve the initial merge - ([691b9db](https://github.com/noirbizarre/over/commit/691b9db0f97cb52f6e617dd7e95241d37afca908))
- Add more tests and documentation - ([4f29b14](https://github.com/noirbizarre/over/commit/4f29b1455baa5cf4956397c7a6f1d61c955204e6))
- Remove unused files - ([876107e](https://github.com/noirbizarre/over/commit/876107ea1485b7966a0930a07d9e5d6befa5e510))
- Remove empty `auth` package - ([a16bc21](https://github.com/noirbizarre/over/commit/a16bc2199a8633b01f52ff9e4221c6f317ed0713))
- More debug - ([787ac40](https://github.com/noirbizarre/over/commit/787ac40277727d90b0ca7de15d9c501c0446d740))
- Deps - ([5905733](https://github.com/noirbizarre/over/commit/590573381712699be2afbde36cf279f3b99a2ba8))

### Tpl

- Render rust at main - ([b09f1ce](https://github.com/noirbizarre/over/commit/b09f1ce2109713c66cd0a1d8978f4e6003cd854c))

### Wip

- Deps - ([0467bdc](https://github.com/noirbizarre/over/commit/0467bdc51a8676b0b99418ca21e91c3bf61932bb))
- Wip - ([36d443a](https://github.com/noirbizarre/over/commit/36d443a72d86d1223fcc3c711677b6f80e5c8d27))
- Install - ([c2e3fa6](https://github.com/noirbizarre/over/commit/c2e3fa6adc9b2450aafad1cdb444439052282f01))
- Install - ([9a8a600](https://github.com/noirbizarre/over/commit/9a8a6007b83a2ccc5f84d6b5f937709364bba4f4))

