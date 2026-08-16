# LINE OA B2C:複数の公式アカウントとクレジット課金

DuduCloud は1つの gateway 上で複数の顧客の LINE 公式アカウントをホストできます。各 OA はそれぞれ専用の agent に紐づき、クレジット残高を持ちます。顧客側のエンドユーザーはその AI カスタマーサポート agent とチャットし、返信のたびにクレジットが消費されます。

## 複数の OA を設定する

`config.toml`:

```toml
[[channels.line.accounts]]
name              = "acme-support"      # label + credit namespace
channel_token_enc = "…"                 # AES-256-GCM (or channel_token plain)
channel_secret_enc = "…"
agent_id          = "acme-agent"
credit_rate       = 2.0                 # points per 1K output tokens; 0 = off

[[channels.line.accounts]]
name              = "beta-shop"
channel_token_enc = "…"
channel_secret_enc = "…"
agent_id          = "beta-agent"
credit_rate       = 1.5
```

旧来の単一 OA の設定形式(トップレベルの `channel_token` / `channel_secret`)は引き続き使用できます。`default` という名前の1つのアカウントとして解決されるため、既存のデプロイは変更不要です。

## クレジット管理(オペレーター)

ポイントはオペレーターが付与します。課金決済(PayUni によるチャージ)は、オペレーターがゲートする別のフローです。

```bash
duduclaw credit grant acme-support U1234567890 500 --reason "monthly plan"
duduclaw credit balance acme-support U1234567890
duduclaw credit history acme-support U1234567890
```

課金方式:各返信は `ceil(output_tokens / 1000 * credit_rate)` ポイントを消費します。ユーザーの残高がゼロに達し、課金が有効(`credit_rate > 0`)な場合、返信は LLM 呼び出しの**前**に拒否され(fail-closed)、ユーザーにはチャージの案内が表示されます。`credit_rate` を 0 にすると、その OA の課金は無効になります。

## 現状

設定モデル、クレジット台帳、オペレーター CLI はすでに用意され、ユニットテストも通過しています。残る統合作業は、共有の `/webhook/line` endpoint を LINE の `destination` フィールドでルーティングするように配線し(アカウントごとの署名検証、不一致は fail-closed)、返信のたびにゲートと控除を行うことです。それまでは、単一の OA は従来の経路で動作します。
