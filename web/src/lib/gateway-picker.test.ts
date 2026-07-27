import { describe, expect, it } from 'vitest';
import { decideAction, type GatewayRecord } from './gateway-picker';

function rec(url: string, name = 'GW'): GatewayRecord {
  return { name, host: 'h', port: 18789, version: '1.45.0', tls: false, url };
}

describe('decideAction (auto-selection policy, design §2.5)', () => {
  it('auto-connects a healthy remembered gateway (no picker)', () => {
    const remembered = rec('http://gw:18789/');
    const action = decideAction({
      remembered,
      rememberedHealthy: true,
      discovered: [rec('http://other:18789/')], // ignored when remembered is healthy
    });
    expect(action).toEqual({ kind: 'auto', target: remembered, from: 'remembered' });
  });

  it('falls to the picker when the remembered gateway is unreachable', () => {
    const action = decideAction({
      remembered: rec('http://gw:18789/'),
      rememberedHealthy: false,
      discovered: [], // even with nothing discovered, show the picker (user can act)
    });
    expect(action).toEqual({ kind: 'list' });
  });

  it('auto-connects when exactly one gateway is discovered and none remembered', () => {
    const only = rec('http://office:18789/');
    const action = decideAction({ remembered: null, rememberedHealthy: false, discovered: [only] });
    expect(action).toEqual({ kind: 'auto', target: only, from: 'discovered' });
  });

  it('shows the list when multiple gateways are discovered and none remembered', () => {
    const action = decideAction({
      remembered: null,
      rememberedHealthy: false,
      discovered: [rec('http://a:18789/'), rec('http://b:18789/')],
    });
    expect(action).toEqual({ kind: 'list' });
  });

  it('starts local when nothing is remembered and nothing is discovered', () => {
    const action = decideAction({ remembered: null, rememberedHealthy: false, discovered: [] });
    expect(action).toEqual({ kind: 'local' });
  });
});
