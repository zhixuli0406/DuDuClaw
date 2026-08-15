import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { SecretSourceField, parseSecretSource, composeSecretSource } from './SecretSourceField';

function Fixture({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  return <SecretSourceField value={value} onChange={onChange} placeholder="Paste your token..." />;
}

describe('parseSecretSource / composeSecretSource', () => {
  it('round-trips an env reference', () => {
    const parsed = parseSecretSource('secret://env/TELEGRAM_BOT_TOKEN');
    expect(parsed.mode).toBe('env');
    expect(parsed.envName).toBe('TELEGRAM_BOT_TOKEN');
    expect(composeSecretSource('env', parsed)).toBe('secret://env/TELEGRAM_BOT_TOKEN');
  });

  it('round-trips a keychain reference with an explicit service', () => {
    const parsed = parseSecretSource('secret://keychain/duduclaw/telegram');
    expect(parsed.mode).toBe('keychain');
    expect(parsed.keychainService).toBe('duduclaw');
    expect(parsed.keychainAccount).toBe('telegram');
    expect(composeSecretSource('keychain', parsed)).toBe('secret://keychain/duduclaw/telegram');
  });

  it('drops the service segment when only an account was ever given (default-service form)', () => {
    const parsed = parseSecretSource('secret://keychain/telegram');
    expect(parsed.keychainService).toBe('');
    expect(parsed.keychainAccount).toBe('telegram');
    expect(composeSecretSource('keychain', { keychainAccount: 'telegram' })).toBe('secret://keychain/telegram');
  });

  it('round-trips a file reference, keeping the absolute path (with its leading slash) intact', () => {
    const parsed = parseSecretSource('secret://file//run/secrets/tg_token');
    expect(parsed.mode).toBe('file');
    expect(parsed.filePath).toBe('/run/secrets/tg_token');
    expect(composeSecretSource('file', parsed)).toBe('secret://file//run/secrets/tg_token');
  });

  it('treats vault / 1Password / Infisical URIs as raw "secrets manager" mode, unparsed', () => {
    for (const uri of ['secret://vault/kv/data/telegram', 'secret://onepassword/tg', 'secret://infisical/tg']) {
      const parsed = parseSecretSource(uri);
      expect(parsed.mode).toBe('manager');
      expect(parsed.raw).toBe(uri);
      expect(composeSecretSource('manager', parsed)).toBe(uri);
    }
  });

  it('treats anything without a secret:// prefix as direct plaintext', () => {
    const parsed = parseSecretSource('sk-ant-live-abc123');
    expect(parsed.mode).toBe('direct');
    expect(parsed.raw).toBe('sk-ant-live-abc123');
    expect(composeSecretSource('direct', parsed)).toBe('sk-ant-live-abc123');
  });

  it('composes an empty string when a guided mode has no value typed yet', () => {
    expect(composeSecretSource('env', {})).toBe('');
    expect(composeSecretSource('keychain', {})).toBe('');
    expect(composeSecretSource('file', {})).toBe('');
  });
});

describe('<SecretSourceField>', () => {
  it('composes a secret://env reference from a typed variable name', async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderWithProviders(<Fixture value="" onChange={onChange} />);

    await user.click(screen.getByRole('combobox', { name: 'Credential source' }));
    await screen.findByRole('listbox');
    await user.click(screen.getByRole('option', { name: 'Environment variable' }));

    await user.type(screen.getByPlaceholderText('TELEGRAM_BOT_TOKEN'), 'MY_TOKEN');

    expect(onChange).toHaveBeenLastCalledWith('secret://env/MY_TOKEN');
  });

  it('composes a secret://keychain reference from service + account fields', async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderWithProviders(<Fixture value="" onChange={onChange} />);

    await user.click(screen.getByRole('combobox', { name: 'Credential source' }));
    await screen.findByRole('listbox');
    await user.click(screen.getByRole('option', { name: 'OS keychain' }));

    await user.type(screen.getByPlaceholderText('duduclaw (leave blank for the default)'), 'duduclaw');
    await user.type(screen.getByPlaceholderText('telegram'), 'bot-token');

    expect(onChange).toHaveBeenLastCalledWith('secret://keychain/duduclaw/bot-token');
  });

  it('reverse-parses an existing secret://keychain value back into guided fields on mount', () => {
    renderWithProviders(<Fixture value="secret://keychain/duduclaw/telegram" onChange={vi.fn()} />);

    expect(screen.getByRole('combobox', { name: 'Credential source' })).toHaveTextContent('OS keychain');
    expect(screen.getByDisplayValue('duduclaw')).toBeInTheDocument();
    expect(screen.getByDisplayValue('telegram')).toBeInTheDocument();
  });

  it('reverse-parses an existing secret://env value back into "type it in" — no, into env mode with the name shown', () => {
    renderWithProviders(<Fixture value="secret://env/DISCORD_TOKEN" onChange={vi.fn()} />);

    expect(screen.getByRole('combobox', { name: 'Credential source' })).toHaveTextContent('Environment variable');
    expect(screen.getByDisplayValue('DISCORD_TOKEN')).toBeInTheDocument();
  });

  it('clears the field instead of carrying over a stale value when the source is switched', async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderWithProviders(<Fixture value="" onChange={onChange} />);

    // Type a value in the default "type it in" mode first.
    await user.type(screen.getByPlaceholderText('Paste your token...'), 'plain-text-secret');
    expect(onChange).toHaveBeenLastCalledWith('plain-text-secret');

    // Switch source — the field must not silently reinterpret the old text
    // as a path, a variable name, or anything else.
    await user.click(screen.getByRole('combobox', { name: 'Credential source' }));
    await screen.findByRole('listbox');
    await user.click(screen.getByRole('option', { name: 'Mounted file' }));

    expect(onChange).toHaveBeenLastCalledWith('');
    expect(screen.getByPlaceholderText('/run/secrets/telegram_token')).toHaveValue('');
    // The old "type it in" field is gone entirely, not just visually stale.
    expect(screen.queryByPlaceholderText('Paste your token...')).not.toBeInTheDocument();
  });

  it('passes a secrets-manager URI through verbatim without wrapping it further', async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderWithProviders(<Fixture value="" onChange={onChange} />);

    await user.click(screen.getByRole('combobox', { name: 'Credential source' }));
    await screen.findByRole('listbox');
    await user.click(screen.getByRole('option', { name: 'Secrets manager' }));

    await user.type(
      screen.getByPlaceholderText('secret://vault/kv/data/telegram'),
      'secret://vault/kv/data/tg',
    );

    expect(onChange).toHaveBeenLastCalledWith('secret://vault/kv/data/tg');
  });
});
