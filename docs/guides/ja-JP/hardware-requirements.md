# DuDuClaw OS ハードウェア要件と互換性ガイド

DuDuClaw OS は **x86-64 の Agent-Native OS** です。一般的な市販PC、自作PC、ミニPCで動作し、特別なハードウェアも単体GPUも必要ありません。2015年以降の主流なx86マシンであれば、ほぼ動作します。

本ドキュメントではハードウェアを**2つの層**に分けて説明します。これは全体を理解するうえで最も重要な枠組みなので、混同しないでください。

1. **DuDuClaw OS本体を動かせるハードウェア**——x86-64 + AVX2（x86-64-v3）+ UEFI + SSD。市販ミニPC、自作PC、さらにはほとんどのx86ノートPCも、この条件を満たせば対応します。
2. **OSは動かせないが、DuDuClawのエコシステムで明確な役割を持つハードウェア**——Raspberry PiのようなARM SBCや、ESP32/Arduinoのようなマイクロコントローラー（MCU）です。これらの役割は**センサーエンドポイント**です。DuDuClawのresident sensing（`[[tick.sources]]`）を通じて、x86ハードウェア上で動くagentにデータを送る側であり、OS本体を動かす側ではありません。

まず「必須要件」（満たさなければ起動しません）を確認し、そのあと用途に応じてスペック早見表や互換性チェックリストを見てください。

## 目次

- [必須要件](#必須要件3項目どれか1つでも欠けると起動しません)
- [スペック早見表](#スペック早見表)
- [自作PC互換性チェックリスト](#自作pc互換性チェックリスト)
- [x86ノートPCの評価](#x86ノートpcの評価)
- [おすすめの市販ハードウェア](#おすすめの市販ハードウェアそのまま買えるもの)
- [既知のドライバー不足](#既知のドライバー不足選定時に避けるもの)
- [非x86ハードウェアとIoTエンドポイント（Raspberry Pi／Arduino／ESP32）](#非x86ハードウェアとiotエンドポイントraspberry-piarduinoesp32)
- [二層構成の全体マトリクス](#二層構成の全体マトリクス)
- [Mac／ARMエミュレーションで検証できない理由](#macarmエミュレーションで検証できない理由)
- [起動メディアの正しい書き込み方](#起動メディアの正しい書き込み方)

## 必須要件（3項目、どれか1つでも欠けると起動しません）

1. **AVX2に対応したx86-64 CPU**（＝ x86-64-v3ベースライン：AVX、AVX2、BMI1、BMI2、F16C、FMA、LZCNT、MOVBE、OSXSAVE）
   - Intel：Haswell（2013年、第4世代Core）以降。Atom系のCeleron/Pentium Nシリーズは**Gracemont**（Alder Lake-N、N100/N200/N305など、2023年以降）にならないと対応せず、それより前のGoldmont/Goldmont Plus/Tremont世代はAVX2を持たないことがあります（詳細と確認方法は下の「自作PC互換性チェックリスト」を参照）。
   - AMD：**Excavator**（2015年、モバイル向けAPU）以降、続くZen/Zen+/Zen 2/Zen 3/Zen 4/Zen 5（2017年以降）はすべて対応しています。それより前のBulldozer/Piledriver/Steamroller世代（FXシリーズのデスクトップ機、一部のAPU、2011〜2014年）は「x86-64」を名乗っていても**AVX2/BMI2に対応していません**。
   - ❌ **arm64は非対応です**：Apple Silicon Mac、Raspberry Pi、ARMミニPCなどが含まれます。OS imageはx86-64向けにビルドされているため、ARMマシン（またはARM MacでのUTM/QEMUエミュレーション）では起動しないか、動いても極端に遅く、サポート対象の経路ではありません。
2. **UEFI起動**（2012年以降のマザーボードにはほぼ搭載されています）
   - A/Bアトミック更新とsystemd-bootはUEFIに依存しています。従来のBIOS-onlyマシンは非対応です。
   - Secure Bootは現時点ではオフでも構いません（自己署名チェーンはまだ必須化されていません）。
3. **SSD**（A/B更新とgpuiデスクトップはどちらもI/Oに敏感です。HDDだと動作がもたつきます）

## スペック早見表

| 項目 | 最低 | 推奨 | 快適（ゲーム／ローカルLLM） |
|---|---|---|---|
| **CPU** | x86-64-v3（AVX2必須）：Intel Haswell以降 / AMD Excavator以降 | Intel N100 / N305、AMD Ryzen 5 | AMD Ryzen 7 8845HS |
| **メモリ** | 4 GB | 8 GB | 16 GB以上 |
| **ストレージ** | 64 GB SSD | 128 GB SSD | 256 GB以上のSSD |
| **ディスプレイ／GPU** | 単体GPU不要（内蔵GPUで十分） | 内蔵GPU | 内蔵GPU／単体GPU |
| **ファームウェア** | UEFI | UEFI | UEFI |
| **ネットワーク** | 有線またはWi-Fi（iwd） | — | — |

> **なぜ単体GPUが不要なのか。** DuDuClaw OSは現時点で**llvmpipeによるソフトウェアレンダリング**（CPUがデスクトップを描画します）を採用しています。これが市販ハードウェアに特にやさしい理由です——単体GPUを持たない最安のミニPCでも、フルのグラフィカルデスクトップが動きます。内蔵GPU／単体GPUがあるのは将来のハードウェアアクセラレーションのための余地であり、必須ではありません。

## 自作PC互換性チェックリスト

自作PC（DIYで組み立てたマシン）は、上記の必須要件を満たせば対応します。「認証リスト」のようなものは存在しません——ただし実際の組み立てではいくつかよくある落とし穴があるので、1つずつ確認してください。

| チェック項目 | 通過条件 | よくある落とし穴 |
|---|---|---|
| **マザーボード／チップセット** | Intel 8シリーズチップセット（LGA1150、Haswell対応）以降、またはAMD AM4／AM5（2017年以降）。マザーボード自体にUEFIが搭載されている必要があります | 古すぎるBIOS-onlyの旧型ボード（2012年以前）は完全に非対応です |
| **CPU** | 上記「必須要件」のIntel/AMD世代表を参照 | 中古の古いAMD FX（Bulldozer/Piledriverシリーズ）、古いNASでよく使われる低価格帯のAtom/Celeron Jシリーズ（Bay Trail/Cherry Trail/Goldmont世代）はAVX2を持たないことがあり、「x86-64」であっても起動しません。**購入前に必ず`lscpu`かCPU-Zでflagsに`avx2`が含まれるか確認してください**——CPUのブランドや発売年だけで判断しないこと |
| **GPU** | 単体GPU不要（llvmpipeソフトウェアレンダリング） | 単体GPU（NVIDIA/AMD/Intel Arc）があっても問題は起きませんが、現時点では自動的にアクセラレーションには使われません——将来のための余地があるだけです |
| **有線NIC** | Intel（I219/I225/I226など）、Realtekの主流チップ（RTL8111/8168シリーズ）はLinuxのサポートが成熟しています | **RTL8125（2.5GbE）**：カーネル内蔵の`r8169`ドライバーに基本的なサポートはマージされていますが、コミュニティからはRealtek公式のout-of-tree `r8125` DKMSに比べて安定性が劣るという報告が繰り返し寄せられています（[Arch Linuxフォーラム](https://bbs.archlinux.org/viewtopic.php?id=262120)）。選定時は避けるか、DKMSモジュールを事前に用意しておいてください |
| **Wi-Fi NIC** | Intel AXシリーズ、MediaTekの`mt7921`／`mt7925`シリーズはメインラインのサポートが良好です | **MT7927（Wi-Fi 7チップ）**：確認時点でも**メインラインドライバーが存在せず**、`mt7925e`はそのPCI IDへのバインドを明確に拒否します。コミュニティは`mediatek-mt7927-dkms`のようなout-of-treeパッケージで補うしかありません（[jetmブログ](https://jetm.github.io/blog/posts/mt7927-wifi-the-missing-piece/)）。購入前に`lspci -nn`でM.2カードの実際のチップを確認してください。マザーボードのスペック表だけを信じないこと |
| **ストレージ** | NVMeまたはSATA SSDのどちらでも可 | HDDは動作がもたつきます（A/B更新とデスクトップの両方がI/Oに敏感なため）。推奨しません |
| **起動モード** | マザーボードのBIOS/UEFI設定内で「Boot Mode」を**UEFI**に設定する必要があります（CSM/Legacyではなく） | 多くのマザーボードは出荷時にCSM互換モードがオンになっているため、手動で純粋なUEFIに切り替える必要があります。「Fast Boot」／「Ultra Fast Boot」オプションはUSB起動メニューをスキップすることがあるため、USB起動メディアを焼く前にオフにしておくことを推奨します |
| **Secure Boot** | オン／オフどちらでも可 | 自己署名チェーンはまだ必須化されていませんが、一部のマザーボードは出荷時にオンになっています。起動に失敗する場合は、まずBIOSでオフにして切り分けてください |

## x86ノートPCの評価

技術的には、必須要件（x86-64 + AVX2 + UEFI + SSD）を満たすノートPCであればDuDuClaw OSは動作します——ただし**DuDuClaw OSは現時点でノートPCに特化した互換性検証を行っていません**（applianceプロジェクトが対象とするハードウェアはデスクトップのミニPCです。下記のおすすめハードウェアを参照）。以下はLinuxノートPCサポートに関する一般的な既知のリスクであり、DuDuClawによる実機検証の結果ではありません。

- **電源管理／ACPI**：バッテリー持続時間、ファンカーブ、蓋を閉じたときのサスペンドなどの挙動は、メーカー独自のACPIパッチが必要になることが多いです。コミュニティのサポートが手厚いブランド（ThinkPad、Tuxedoなど）はリスクが低く、ゲーミング機や超薄型モデルはリスクが高くなります。
- **Wi-Fi／Bluetooth**：ノートPCはデスクトップのM.2カードとは異なる型番のMediaTek/Qualcomm/Realtek専用モジュールを内蔵していることが多く、MT7927のようなメインラインドライバーの欠落に当たる可能性が同様にあります。起動後は必ず`lspci -nn`で確認してください。
- **タッチパッド**：近年のPrecision Touchpad（I2C HID）は最近のカーネルでサポートが成熟しています。古いPS/2エミュレーション方式のタッチパッドはジェスチャーやマルチタッチが不完全な場合があります。
- **Secure Boot**：ほとんどのOEMノートPCは出荷時にオンになっているため、USB起動でインストールするにはBIOS/UEFIに入って自分でオフにする必要があります。
- **タッチスクリーン、指紋認証、HDRパネル**などの高度な機能はDuDuClaw OSの設計対象外であり、動作は保証されません。

結論：ノートPCでDuDuClaw OSを動かすことは技術的には可能ですが、「自己責任で検証する」レベルのものです——唯一のシステムドライブを直接上書きせず、まずUSB起動で試してみることを推奨します。

## おすすめの市販ハードウェア（そのまま買えるもの）

| クラス | 機種例 | 説明 |
|---|---|---|
| 💰 予算重視 | Intel N100ミニPC（Beelink／GMKtec、約NT$4,000〜6,000） | ファンレスで省電力。デスクトップ用途＋オフィスワーク＋ネット閲覧には十分 |
| ⚖️ バランス型 | Beelink SERシリーズ（Ryzen）、Intel NUC | 性能と価格のバランスが良い |
| 🎯 ターゲット機種 | Intel N305／AMD Ryzen 8845HSミニPC | プロジェクトが対象とするハードウェア。ゲーム／ローカルLLMもより余裕を持って動きます |

## 既知のドライバー不足（選定時に避けるもの）

以下のハードウェアは現時点で**ドライバーサポートが不十分**です。選定時に避けるか、互換性のあるNICをもう1枚予備で用意してください。

- **MT7927 Wi-Fi 7チップ**——確認時点でもメインラインドライバーは**存在しません**。`mt7925e`はそのPCI IDへのバインドを拒否しており、唯一の解決策はコミュニティ製のDKMSパッケージを事前に用意しておくことです（[jetmブログ](https://jetm.github.io/blog/posts/mt7927-wifi-the-missing-piece/)、[GitHub](https://github.com/danmeedev/mt7927-bazzite)）。これは「自分でカーネルをメンテナンスするかどうか」とは無関係です——ディストリビューションの標準パッケージを使っても同様に認識されません。上流にまだドライバー自体が書かれていないためです。
- **RTL8125 2.5G有線NIC**——カーネル内蔵の`r8169`には基本的なサポートがすでにマージされています（[LKML](https://lkml.kernel.org/netdev/de076b11-2523-4116-ec08-b7e331497509@gmail.com/)）が、複数のコミュニティ報告でRealtek公式のout-of-tree `r8125` DKMSより安定性が劣るとされています（[Archフォーラム](https://bbs.archlinux.org/viewtopic.php?id=262120)）。マージされた正確なバージョン番号と現在の安定性の差については**信頼できる一次情報源が見つかりませんでした**。未検証として明記します。実機でのテストが必要です。

購入前にマザーボード／ミニPCが使用しているネットワークチップの型番を確認し（`lspci -nn`）、上記2つのチップを避けるか、少なくとも追加でDKMSモジュールが必要になる可能性を頭に入れておくことをおすすめします。

## 非x86ハードウェアとIoTエンドポイント（Raspberry Pi／Arduino／ESP32）

このセクションで扱うハードウェアは**すべてDuDuClaw OS本体を動かせません**。しかしそれは役に立たないという意味ではありません——DuDuClawのエコシステムにおいて明確な第二層の役割、すなわち**センサーエンドポイント**を担っています。

### Raspberry Pi

**なぜDuDuClaw OSを動かせないのか**：Raspberry Pi 4はCortex-A72、Raspberry Pi 5はCortex-A76を採用しており、どちらも64bitの**ARMv8-A（aarch64/arm64）アーキテクチャ**であり、x86-64ではありません（[Raspberry Pi 5のスペック、Wikipediaでクロスチェック済み](https://en.wikipedia.org/wiki/Raspberry_Pi)：4コアのCortex-A76 @ 2.4GHz、RAMは1/2/4/8/16GBから選択可）。DuDuClaw OS imageは純粋にx86-64向けにビルドされています。これは**命令セットの非互換性**であり、ドライバーの欠落ではありません——「Apple Silicon Macで起動できない」（下記セクション参照）のとまったく同じ理由です。ソフトウェアの更新では解決できず、ARM64向けのimageを別途ビルドするしかありません。

Raspberry Piの起動の仕組みもx86 PCとは異なります。デフォルトではBroadcom独自のクローズドソースGPUファームウェア＋device-treeによる起動フローを使っており、**標準的なPCのような内蔵UEFIを持ちません**（Pi 4/5にはオプションのRaspberry Pi UEFIファームウェアを導入できますが、それは別プロジェクトであり、標準搭載ではありません）。これは「将来ARM64ビルドを出すかどうか」を検討する際に重要なポイントです。

**将来ARM64ビルドを出す場合のコスト評価**（誠実に作業項目を列挙します。架空の工数は出しません）：

- この道はゼロからのスタートではありません——DuDuClawの旧版（現在は凍結済み）のDebian/mkosi appliance構築パイプラインには、もともと`APPLIANCE_ARCH=arm64`という「smoke-build」経路が存在していました（`appliance/mkosi.conf.d/10-arch-arm64.conf`）。しかしその経路の用途は「Apple Silicon上でのローカルビルド＋QEMUによるスモークテスト」と明記されており、そのファイル自体にも「x86-64 remains the shipping target」と注記されています——つまり使っているのは**汎用のarm64 Debianカーネル**であり、実機のARMハードウェア（ましてやRaspberry Pi）で起動検証されたことは一度もありません。「Raspberry Piをサポートする」こととは別の話です。
- 現在の主力ビルドパイプライン（Yocto／`meta-duduclaw`）は完全にx86-64向けのmachine設定（`duduclaw-genericx86-64.conf`／`duduclaw-qemux86-64.conf`）しか持っておらず、arm64やRaspberry Pi専用のmachine定義はまだ存在しません。
- 実際にRaspberry Piをサポートするとなると、作業量はおおよそ次のブロックに分かれます。①まったく新しいYocto machine BSP（Yoctoエコシステムには`meta-raspberrypi`のような上流BSPレイヤーがすでにあり出発点にはなりますが、本プロジェクトのimage／更新パイプラインへの統合はこれから必要です）②Raspberry Pi専用の起動チェーン——標準的なUEFIがないため、別途Pi UEFIファームウェアを使うかU-Bootに切り替える必要があり、既存のUEFI＋systemd-bootによるA/Bデュアルスロット更新設計がこの経路に適用できるか再検証が必要です③グラフィックススタック（現状x86-64ではllvmpipeソフトウェアレンダリング）をaarch64上で一連のチェーンごと再検証する必要があります④x86-64-v3向けのCPUチューニングはARMにはまったく適用できないため、独立したtune／sstateキャッシュを新設する必要があり、ビルドマトリクスがそのまま倍増します⑤「快適」ティアの目玉であるSteam／ゲーミング用途はARM Linuxでは公式サポートがなく、この機能自体を丸ごと外すことになります⑥Raspberry Pi専用のBroadcomディスプレイ／Wi-Fiファームウェアとドライバーは別途調査・統合が必要です。
- 全体として「新しいプラットフォームターゲットを1つ丸ごと追加する」規模の工数であり、「ビルドオプションを1つ追加する」規模ではありません。正確な工数を出せる信頼できる根拠がないため、ここではあえて作業項目だけを列挙し、架空の数字は出していません。

**DuDuClawのエコシステムにおけるRaspberry Piの正しい位置づけ**：標準的なLinux（Raspberry Pi OS、Debian arm64など）を動かし、**エッジagentノードまたはIoTゲートウェイ**として使います——Raspberry Pi上に小さなサービスを書いてセンサー／GPIO／カメラの状態を読み取り、HTTPで公開し、x86ホスト上で動くDuDuClaw gatewayが`[[tick.sources]]`経由でそのデータを取り込みます（実際の接続方法は下記「センサーエンドポイントをDuDuClawのresident sensingにつなぐ方法」と同じで、LANのプライベートIPに関する制限も含みます）。複数のArduino/ESP32（BLE/LoRa/Zigbeeなどの近距離プロトコル経由）を集約するノードとしても適しており、外部に提供するエンドポイントを1つにまとめることで、DuDuClaw側が管理するソース数を減らせます。

### Arduino／ESP32

**なぜマイクロコントローラー（MCU）であり、OSを動かせるコンピューターではないのか**：

- **Arduino Uno**：中核チップはATmega328Pで、**8bit**のMCUです。**2 KBのSRAM**、32 KBのフラッシュ、16 MHz（[Arduino公式ドキュメント](https://docs.arduino.cc/hardware/uno-rev3/)）。これは純粋なマイクロコントローラーであり、「軽量なLinuxを1つ動かすことすら不可能」です——これは**アーキテクチャの非互換性**の問題ではなく、**リソースレベル**の問題です。仮にx86命令セットに置き換えたとしても、2KBのメモリでは現代的なOSはどれも動きません。
- **ESP32**：Xtensa LX6（一部の新しいモデルはLX7またはRISC-V）、多くはデュアルコアで240MHz、内蔵SRAMは型番によって約256〜768 KiB（オリジナルのESP32は520 KiB）、**MMUなし**（[Wikipedia、ESP32](https://en.wikipedia.org/wiki/ESP32)）。ネイティブではFreeRTOSかベアメタル（ESP-IDF SDK）しか動かず、Linuxには対応していません——新しいモデルでPSRAMを追加して数MBまで拡張しても、現代的なLinuxデスクトップ環境が必要とするメモリ量とMMUによる仮想メモリ管理には遠く及びません。

両者に共通する正しい位置づけは**センサーエンドポイント**です——Wi-Fi（ESP32はネイティブ対応）または追加のネットワークモジュール（Arduinoはよく ESP8266/ESP32拡張ボードと組み合わせます）でセンサー値を送信し、上位のデバイス（Raspberry Piやx86ホスト）がそれを取得・受信します。

### センサーエンドポイントをDuDuClawのresident sensingにつなぐ方法

誤解されやすいが**コードから直接読み取れる**（`crates/duduclaw-gateway/src/tick_config.rs`／`tick_source.rs`／`tick_source_poll.rs`／`tick_source_ws.rs`）アーキテクチャ上の制約を、印象ではなくまず明確にしておきます。

1. **`http_poll`と`websocket`（loopback以外）はどちらも`web_fetch::validate_url`というSSRFゲートを通過します。プライベートアドレス帯のIP（`192.168.x.x`／`10.x.x.x`／`169.254.x.x`など）はその場で拒否されます**——`tick_config.rs`のテスト`ssrf_urls_disable_the_source`は、`http://192.168.1.10/x`のようなアドレスが必ず拒否されることを明示的に検証しています。つまり**DuDuClaw gatewayは、家庭やオフィスのLAN内に置かれたESP32を直接`http_poll`することができません**——これは意図的なセキュリティ境界であり、バグや欠陥ではありません。
2. **`websocket`というtick kindの本質はWSクライアントであり、自分から能動的に接続しに行くものです**。WSサーバーではありません（`tick_source_ws.rs`の`connect_source`を参照）。したがって「ESP32が能動的にDuDuClawへプッシュする」という直感的な説明は正確ではありません——正しくは「DuDuClawがESP32（または何らかのrelay）が開いているWSサーバーへ接続しに行き、接続後はいつフレームをプッシュするかを相手側が決める」という形です。しかもloopback以外の`wss://`も同様に上記のSSRFゲートを通過するため、LANのプライベートIPは同じように遮断されます。

したがって、**LAN内のIoTデバイスを実務的に接続する合法な方法は3通りあります**：

1. **`command`**（`config.toml`で`[tick] allow_command_sources = true`を先に有効化する必要があり、デフォルトはオフでfail-closedです）：operatorが自分で書いたargv（例えば`curl`やセンサーAPIを呼び出すスクリプト）にLANデバイスへアクセスさせます——このサブプロセスのネットワーク呼び出しは、tick_source自身のSSRFゲート（このゲートは`http_poll`ブランチにしか適用されません）を**経由しない**ため、プライベートIPに合法的にアクセスできます。現時点で最も直接的なLANセンサー接続方法です。
2. **`file_tail`**：DuDuClaw gatewayを動かしているホスト上に別途cron／systemdタイマーのスクリプトを組み、定期的にLANデバイスのAPIを呼び出し、結果を1行ずつのJSONとしてローカルのログファイルにappendします。`file_tail`はファイルを読むだけでネットワークにはいっさい触れないため、当然SSRFゲートの制限を受けません。
3. **`websocket`＋ローカルrelay（loopback）**：gatewayと同じホスト上に小さなrelayサービスを別途立てます（ESP32のMQTT publishを受け取る、または逆にポーリングする）。relay自身が`ws://127.0.0.1:PORT`でWSサーバーを開き、DuDuClawの`websocket` tick sourceがそのloopbackアドレスへ接続します——これはコードのコメントに明記されている「documented local-relay path」という設計意図そのものです。

**実例：ESP32温湿度センサー → tick → autopilotによる自動反応**

1. ESP32にDHT22を接続し、ファームウェアに極小のHTTPサーバーを実装します。`GET /sensor`は`{"temp":32.1,"humidity":58}`を返します。
2. gatewayホスト上にcronスクリプトを組み（1分ごとに実行）、`command`／`file_tail`のいずれかの経路でこのAPIにアクセスします。`file_tail`を使う場合：

   ```bash
   # cron：1分ごとに読み取った値をログにappendする
   curl -s http://192.168.1.50/sensor >> /var/log/duduclaw/esp32-temp.jsonl
   echo >> /var/log/duduclaw/esp32-temp.jsonl
   ```

3. `config.toml`の設定：

   ```toml
   [tick]
   enabled = true

   [[tick.sources]]
   id = "esp32-temp"
   kind = "file_tail"
   path = "/var/log/duduclaw/esp32-temp.jsonl"
   json_fields = { temp = "/temp", humidity = "/humidity" }
   ```

   （外部cronを省いてDuDuClaw自身に取得させたい場合は、代わりに`command`を使います：`kind = "command"`、`command = ["curl", "-s", "http://192.168.1.50/sensor"]`、`interval_secs = 60`とし、`[tick]`に`allow_command_sources = true`を追加します。）

4. 新しい行が入るたびに、DuDuClawは`{temp, humidity}`に`prev_temp`／`delta_temp`／`pct_temp`を自動的に付加し、`AutopilotEvent::Tick{source:"esp32-temp", fields:{...}}`としてbroadcastします（純粋にRust側での処理で、LLMコストはゼロです）。
5. autopilotのルールはそのまま次のように書けます：

   ```json
   {
     "trigger_event": "tick",
     "conditions": {
       "all": [
         { "field": "source", "op": "eq", "value": "esp32-temp" },
         { "field": "temp", "op": "gt", "value": 30 }
       ]
     },
     "action": {
       "type": "notify",
       "channel": "telegram",
       "chat_id": "...",
       "text": "機房溫度 {temp}°C 超過 30 度，請檢查空調"
     }
   }
   ```

温度が閾値を超えるとagentが自動でメッセージを送信します——これこそがresident sensingの設計思想です：System 1（安価で常駐し、決定論的なRustのルール）がフィルタリングを担当し、System 2（クラウドagent）はルールがヒットしたときだけ起こされます。本当に判断が必要になるまでLLMは呼び出されません。

## 二層構成の全体マトリクス

### 第1層：DuDuClaw OS本体を動かせるハードウェア（x86-64ハードウェア）

| ハードウェアクラス | サポート状況 | エコシステムでの役割 |
|---|---|---|
| 自作x86 PC（Intel Haswell以降／AMD Excavator以降、UEFI、SSD） | ✅ 完全対応 | ホスト／OS本体 |
| x86市販ミニPC（N100／N305／8845HSなど） | ✅ 完全対応、推奨ティア | ホスト／OS本体 |
| x86ノートPC（必須要件を満たすもの） | ⚠️ 理論上は動作するが専用の検証はしていない | ホスト／OS本体（自己責任） |
| 古いAMD FX/Bulldozerシリーズ、古いAtom（Bay Trail/Cherry Trail/Goldmont世代） | ❌ 非対応（AVX2非搭載） | 該当なし |
| Apple Silicon Mac（ネイティブ） | ❌ 非対応（ARMアーキテクチャ） | 該当なし。開発機としてgatewayを動かすことのみ可能（OS本体ではない） |
| Apple Silicon Mac＋QEMU/UTMによるx86エミュレーション | ⚠️ 技術的には起動するが極端に遅い | 実用は推奨しない。起動画面をざっと確認する程度の用途 |

### 第2層：周辺機器／センサーエンドポイント（OS本体は動かせない）

| ハードウェアクラス | サポート状況 | エコシステムでの役割 |
|---|---|---|
| Raspberry Pi 4／5（ARM64 SBC） | ❌ DuDuClaw OSを動かせない（ARMアーキテクチャが非互換） | ✅ エッジagentノード／IoTゲートウェイとして利用可。`http_poll`／`command`／`file_tail`経由でデータを送信 |
| その他のARM Linux SBC（原理は同じ、型番ごとの個別検証はしていない） | ❌ 同上 | ✅ 同上 |
| ESP32シリーズ（Wi-Fi/BLE MCU） | ❌ Linuxを一切動かせない（MMUなし、SRAMはKB単位） | ✅ センサーエンドポイント。`command`／`file_tail`／loopbackの`websocket` relay経由で接続 |
| Arduino Uno／Nanoなどの8bit MCU | ❌ 同上（リソースレベルはさらに低い） | ✅ 同上。通常はネットワークモジュール（ESP8266拡張ボードなど）と組み合わせないとネットに接続できない |

## Mac／ARMエミュレーションで検証できない理由

DuDuClaw OSはx86-64です。Apple Silicon Mac上でUTM/QEMUを使ってx86-64をエミュレーションするのは**ソフトウェアエミュレーション（TCG）**であり、動作は遅く、表示や入力もスムーズとは限りません。「起動画面をざっと確認する」程度の用途には向いていますが、**実用には向きません**。実際の体験を得るには、USBに書き込んでx86 UEFIの実機で起動する必要があります。それがネイティブ速度です。

## 起動メディアの正しい書き込み方

リリースごと、machineごとに3つの成果物(ディスク全体image1つと、live インストーラー ISO 2種類——ベースイメージを書き込む`installer`とデスクトップ版を書き込む`installer-desktop`)があり、書き込み方は2通りです(成果物の一覧、検証用の公開鍵とコマンドは[ドキュメントサイトのOS README「Quick start」](https://os.duduclaw.dudustudio.monster/docs/os/readme/)を参照してください。書き込む前に必ず`.minisig`と`.sha256`を検証してください)：

- **インストーラー `.iso`(推奨)**：どちらのISOもbalenaEtcherまたは`dd`でUSBに書き込むか、ディスクに焼きます。UEFIで起動するとグラフィカルなインストールウィザードに入り、対象のSSDを選んでインストールしたあと再起動します。「ディスク／Boot from ISO」による起動に対応している唯一の成果物形式です。
- **ディスク全体イメージ `.wic.zst`**：`zstd -d`で展開してから、そのまま対象ディスクに書き込みます（またはUSBに書き込んでディスクとして起動することもできます）：

```bash
sudo dd if=duduclaw-os-*.wic of=/dev/rdiskN bs=4m status=progress
# rdiskNは対象デバイスの実際の番号に置き換えてください。まずdiskutil list（macOS）またはlsblk（Linux）で確認し、ディスクを間違えないよう注意してください
```

⚠️ **`.wic`はディスクに焼くことも、「Boot from ISO」／QEMUのcdromで起動することもできません**：GPTディスクimageがディスク（`/dev/sr0`）経由の経路をたどる場合、kernelの`sr`ドライバーの`GENHD_FL_NO_PART`制限により、ディスク上にGPTパーティションノードが作られず、起動チェーンがパーティションを見つけられません。ディスクやISOから起動したい場合は、インストーラー`.iso`を使ってください——それはISO9660のlive環境であり、起動メディア上のパーティションテーブルに依存しません。

## 関連ドキュメント

- [appliance-build.md](appliance-build.md) —— DuDuClaw OS imageの入手とビルド（OS側の系統はDuDuClaw-OSリポジトリへ移動済み）
- [deployment-guide.md](deployment-guide.md) —— デプロイ（サーバー側）
- [features/41-resident-sensing.md](../../features/ja-JP/41-resident-sensing.md) —— resident sensingの完全な機能説明（`http_poll`／`command`／`file_tail`／`websocket`の4種類のソース、SSRF防御、rate cap、delta導出）
