# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 - 2026-09-07

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
- **git** Add per-worktree config support - ([7fc33f9](https://github.com/noirbizarre/over/commit/7fc33f9c8f56d78cba7081435f71de8a0fb514fe))
- **git** Advanced git config - ([4bf119d](https://github.com/noirbizarre/over/commit/4bf119dcdc4ac573d6029ffd660b087703b6ace1))
- **git-over** Initial `git-over` implementation - ([2fb3d2c](https://github.com/noirbizarre/over/commit/2fb3d2cd2f008b10433b72fe608d5f126118810a))
- **help** Add colors to help - ([61004e7](https://github.com/noirbizarre/over/commit/61004e72ad383b0f5e78798e5d4f237758cbc3be))
- **install** Parse config - ([ede64a8](https://github.com/noirbizarre/over/commit/ede64a8b527a86a5fbdea0bfba46c93ffad21deb))
- **link** Basic complicit and overwriting handling - ([540d20e](https://github.com/noirbizarre/over/commit/540d20e83b6e9ce66320cfdf0bd14c5d47d6def1))
- **lint** Show precise diagnostics with file locations and key suggestions ([#47](https://github.com/noirbizarre/over/issues/47)) - ([957803e](https://github.com/noirbizarre/over/commit/957803ee4963273a201ab1920b342b49fc2e1229))
- **lint** Add a `over lint` command - ([faf4851](https://github.com/noirbizarre/over/commit/faf4851a3234f36c0440d12d210b01b71e84434b))
- **list** Implement --tree view for overlay listing ([#72](https://github.com/noirbizarre/over/issues/72)) - ([0996a8b](https://github.com/noirbizarre/over/commit/0996a8bf64f819681b1d88a07358f67296bb3b63))
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
- **cli** Make the `home` expected for sub commands only - ([8c75ca6](https://github.com/noirbizarre/over/commit/8c75ca6a76aac8160ee802709e9092e8312815aa))
- **dependencies** Fix wrong circular dependencies resolution on multiple occurrence of transitive dependency - ([9d43762](https://github.com/noirbizarre/over/commit/9d4376262245dd6188adbdd908a53ff4f5fdb7b5))
- **git** Adapt to git2 0.21 API and align git2_credentials - ([8cb9f14](https://github.com/noirbizarre/over/commit/8cb9f1414ae4029f27ddaf8126dd9e31ebd3652d))
- **git** Set `over.overlay` config when cloning repository - ([d11217c](https://github.com/noirbizarre/over/commit/d11217c36a76873348ae81014fc416522c83a51f))
- **git** Support root level git repository in config - ([b30b693](https://github.com/noirbizarre/over/commit/b30b69399cee9ac2a93c6c0fb64e8f34a3a0a3ce))
- **lint** Resolve clippy manual-checked-ops and useless-borrows warnings - ([f9beb35](https://github.com/noirbizarre/over/commit/f9beb35e3a90d4b84792218995b6a371c5d0cdbb))
- **lint** Resolve clippy warnings, TOML formatting, and trailing whitespace ([#67](https://github.com/noirbizarre/over/issues/67)) - ([f8f107e](https://github.com/noirbizarre/over/commit/f8f107e8513ddf5ff8a7bd8676bde166b671c58a))
- **new** Ignore target if it is the default target - ([d4a2b6f](https://github.com/noirbizarre/over/commit/d4a2b6f608fa11399a39d6c0c555b3edbbe81935))
- **overlay** Skip badly formatted files with warning ([#40](https://github.com/noirbizarre/over/issues/40)) - ([527d839](https://github.com/noirbizarre/over/commit/527d83957c1ae496de061e5fac6addd1e3394440))
- **test** Make git_over_mount_debug_output robust to platform TTY behavior - ([a766223](https://github.com/noirbizarre/over/commit/a766223c5f1f455074b4c47904835e4702224cd4))
- **windows** Resolve 16 Windows-only test failures - ([4ed7c6d](https://github.com/noirbizarre/over/commit/4ed7c6d725c79cbbfc7cdde371ee586ccda0e6a2))
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
- **agents** Add missing modules to Layout tree - ([dfcfb7b](https://github.com/noirbizarre/over/commit/dfcfb7bdc442758cebf05ae7aa8b1191708dec22))
- **agents** Expand prek hooks description to include all checks - ([54836ab](https://github.com/noirbizarre/over/commit/54836ab0cc75220664638423e9b6c49ce8377f58))
- **agents** Remove reference to nonexistent Expect type alias - ([2e8caa6](https://github.com/noirbizarre/over/commit/2e8caa67b101a288ca98dde641ea46e3380b498b))
- **cargo** Fix grammar in package description - ([75f4ec7](https://github.com/noirbizarre/over/commit/75f4ec75979089ed1cf6a068ecdcadba760f2b8a))
- **desired** Fix stale EntryKind reachability claim - ([168fd42](https://github.com/noirbizarre/over/commit/168fd425e787bca7e30c51038870bff808487347))
- **exec** Document the Ctx/&Ctx/&Context parameter convention - ([d17dbdc](https://github.com/noirbizarre/over/commit/d17dbdc4edfd20e8ae3bcb0fc137b736a66512fd))
- **logo** Add an initial logo ([#44](https://github.com/noirbizarre/over/issues/44)) - ([c75d38e](https://github.com/noirbizarre/over/commit/c75d38e849e87b5d5e8d2fe82881410231269eab))
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
- Sweep stale #110-pending rustdoc comments - ([f3b61dd](https://github.com/noirbizarre/over/commit/f3b61dd74f9f5ffab025402d8e74979d13d7ee70))
- Add usage/configuration reference pages and ten ADRs - ([5a0f913](https://github.com/noirbizarre/over/commit/5a0f9131c28246cbcced15a29d8620d98e2babd3))

### 🧪 Tests

- **cli** Improve patch coverage for tracing debug output and no-uses - ([200840f](https://github.com/noirbizarre/over/commit/200840f4a64f64e408c0aa342500d708b15c8796))
- **exec** Adopt rstest for Context::builder flag-combination tests - ([de81068](https://github.com/noirbizarre/over/commit/de81068f3894a2e5102e325a4410714714d52018))
- **git** Cover git2 0.21 Result-based branches touched by the API fix - ([c2e88aa](https://github.com/noirbizarre/over/commit/c2e88aa1ca3f2510101dcba8416f6eea2024a4d2))
- Raise test coverage above 90% ([#45](https://github.com/noirbizarre/over/issues/45)) - ([5216fd4](https://github.com/noirbizarre/over/commit/5216fd49272b3057d7a7639757b9b1ac4cd53613))
- Add unit tests to improve coverage from 75% to 79% ([#43](https://github.com/noirbizarre/over/issues/43)) - ([70b1937](https://github.com/noirbizarre/over/commit/70b19378c7cf65f582ec80611c19f21b000ed4bc))
- Raise coverage - ([ccd8c92](https://github.com/noirbizarre/over/commit/ccd8c9204b0ba5ba9d3446d592545c5632acdb96))
- More tests - ([581f4cd](https://github.com/noirbizarre/over/commit/581f4cd1c4dc058ea5c818f885037fc89fdee478))
- Add missing test file - ([5927089](https://github.com/noirbizarre/over/commit/5927089ca087122f809e99b641532a11d0f66789))

### 🎨 Style

- Standardize anyhow::Context aliasing to Context as _ - ([725bf10](https://github.com/noirbizarre/over/commit/725bf105f67bb9b951155bf73a9c3284053c4e1d))
- Group std::sync::LazyLock with the rest of the std imports - ([e75d690](https://github.com/noirbizarre/over/commit/e75d690156e7a6ec393f5ad689c30ff84c71052d))
- Fix import grouping order (std, external, crate) - ([c7c962d](https://github.com/noirbizarre/over/commit/c7c962d8a5d7f0ef8e787ddecb26c9c4ceac3145))
- Format and lint - ([9c0a349](https://github.com/noirbizarre/over/commit/9c0a349eb34a9595dd1142ca354fd89ebda5be32))

### 🏗️ Build

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

- **docs** Remove the unused Zola site skeleton in favour of Zensical - ([98244bf](https://github.com/noirbizarre/over/commit/98244bf1d6b8fb16195602ad9635a302fbdd648e))
- **git** Make Cargo.lock binary - ([92c4bb2](https://github.com/noirbizarre/over/commit/92c4bb27e2caa4283d80ea5f21ba7d2a17ff0b2f))
- **style** Use console style instead of owo - ([0a41739](https://github.com/noirbizarre/over/commit/0a417399a8ea7e54b4d4c3d26daf2e4692b7d7a0))
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

## ❤️ New Contributors

* @noirbizarre made their first contribution in [#136](https://github.com/noirbizarre/over/pull/136)
* @dependabot[bot] made their first contribution in [#122](https://github.com/noirbizarre/over/pull/122)
* @ made their first contribution
