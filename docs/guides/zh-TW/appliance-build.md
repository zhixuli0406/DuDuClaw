# 建置 DuDuClaw OS image

> **已搬移（2026-09）。** OS image 不再從這個 repo 建置。這頁原本描述的
> Yocto layer、release pipeline，以及已凍結的 Debian/mkosi 值班機
> recipe，現在全部都在獨立的 [DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS)
> repo 裡。這頁保留下來，作為平台文件的穩定入口，告訴你該往哪裡去。

DuDuClaw OS 是把一台小型 x86-64 PC 變成無頭（headless）DuDuClaw 值班機的
開機 image（成品從使用者角度長什麼樣，見
[值班機功能總覽](../../features/zh-TW/50-duduclaw-os-appliance.md)）。你現在
讀的這個平台 repo，放的是 OS 拿去 vendor 成精簡快照的 Rust workspace；
OS 本身有自己的 repo、自己的 release 線、自己的 changelog。

---

## 1. 取得 image

有兩條路：

- **下載已簽章的 release。** [DuDuClaw-OS Releases](https://github.com/zhixuli0406/DuDuClaw-OS/releases)
  上每個 release 都會依機型發布一份整碟 image
  （`duduclaw-os-<machine>-v<ver>.wic.zst`）與一份 live 安裝器 ISO
  （`duduclaw-os-installer-<machine>-v<ver>.iso`），各自附上 `.sha256` 與
  minisign `.minisig`。燒錄前務必驗證；公鑰與確切指令在該 repo 的
  README（「快速開始」／"Quick start"）與 `SECURITY.md` 裡。
- **從原始碼建置。** 把 DuDuClaw-OS clone 到這個 repo 旁邊，照它的 README
  （「從原始碼建置」／"Build from source"）與 `meta-duduclaw/README.md`
  （"Usage"）走：一個 Docker builder 容器、`kas build`，以及
  `scripts/release-os.sh build → smoke → package → publish` 這條 pipeline。
  只有在需要刷新 vendor 進去的 Rust 快照時，才需要這個平台 repo 的
  sibling checkout。

## 2. 你在建置什麼

出貨的 image 是 `meta-duduclaw/` 這個 Yocto layer（Yocto 6.0
"wrynose"，kernel 6.18）產出的 `duduclaw-image-appliance`：A/B 雙槽配置、
支援原子更新與回滾、唯讀 root 由 dm-verity 驗證、自簽 Secure Boot 搭配每個
槽位各自雙簽的 UKI，加上 DuDuClaw gateway ＋ dashboard payload。目前定義了
兩個機型：`duduclaw-qemux86-64`（QEMU bring-up 目標，已驗證可開機）與
`duduclaw-genericx86-64`（真實 x86-64 硬體，已做設定審查；真實硬體開機仍是
待驗證項目）。每個 image 的角色分工與分割區／開機鏈細節記載在 OS repo
裡，這裡不重複。

## 3. Debian/mkosi 那條線

這頁原本記載的 `appliance/` recipe（Debian 13 ＋ mkosi、自安裝 USB
image）已**凍結**：它保留在 DuDuClaw-OS repo 的 `appliance/` 底下，作為
參考與過渡期產物，不出貨、也不再收修正。它自己的 README 仍留著完整開機
流程與待解項目，供想查歷史的人參考。

## 另請參閱

- [DuDuClaw OS 值班機](../../features/zh-TW/50-duduclaw-os-appliance.md)：
  成品從使用者角度長什麼樣、做什麼。
- [硬體需求與相容性](hardware-requirements.md) —— 能跑在什麼硬體上、怎麼
  燒錄開機媒體。
- [DuDuClaw-OS repo](https://github.com/zhixuli0406/DuDuClaw-OS)：layer、
  pipeline、changelog，以及文件索引（`docs/README.md`）。
