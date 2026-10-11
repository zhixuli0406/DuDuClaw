import {describe, it, expect} from 'vitest';
import {COMMERCIAL_CONTACT_URL, COMMERCIAL_CONTACT_NAME, resolveCommercialContact} from './commercial-contact';

describe('authorized general distributor', () => {
  it('always resolves the authorized seller instead of cached arbitrary branding', () => {
    localStorage.setItem('duduclaw-branding-cache', JSON.stringify({branding: {website: 'https://arbitrary.example/shop', company_name: 'Other seller'}, white_label_active: true}));
    expect(resolveCommercialContact()).toEqual({url: 'https://www.futurecorp.tw/', name: COMMERCIAL_CONTACT_NAME.zh});
    expect(COMMERCIAL_CONTACT_URL).toBe('https://www.futurecorp.tw/');
  });
  it('uses the English general distributor name for English UIs', () => {
    expect(resolveCommercialContact('en')).toEqual({url: COMMERCIAL_CONTACT_URL, name: 'Future Corp'});
  });
});
