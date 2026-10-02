# Validate with an explicit account pool

[繁體中文](zh-TW/isolated-validation.md) · [日本語](ja-JP/isolated-validation.md)

A separate DuDuClaw state directory normally still allows the account rotator to detect the host's Claude login or read provider API keys from the process environment. For a validation instance that must use only explicitly configured credentials, add this to that instance's `config.toml`:

```toml
[account_loading]
inherit_host_credentials = false
```

The default is `true`, including when the configuration file does not exist. With `false`, every account-loading caller uses the same policy: the rotator skips `claude auth status`, skips its `ANTHROPIC_API_KEY` loading fallback, and disables provider environment fallbacks during account selection. An empty configured pool therefore remains empty; selection does not repopulate it from ambient keys. Reloading from `true` to `false` removes previously auto-loaded accounts.

Explicit `[[accounts]]` and `[api]` credentials remain available, including encrypted credentials and `secret://env/...` references. An explicitly configured OAuth profile can still use the host profile it names. To validate without credentials, leave those explicit sources out of the validation configuration too.

Invalid TOML, a non-table `account_loading`, a non-boolean `inherit_host_credentials`, or a configuration read error other than a missing file rejects the load, clears the pool and disables ambient fallback. Fix the file and reload it before expecting account selection to succeed.

This setting controls the account rotator's discovery and selection. It does not isolate subprocess environment variables, OS keychains, runtime-specific authentication, filesystem access or networking. Use the runtime's supported container isolation when those boundaries matter. An empty pool verifies the no-account path; provider execution still requires separately authorized, usable credentials and live acceptance evidence.

See [account rotation](../features/07-account-rotation.md) and [Discovery setup](../features/60-discovery.md).
