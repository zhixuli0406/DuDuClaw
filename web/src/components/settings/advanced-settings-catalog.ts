export const ADVANCED_SETTINGS_PATH = '/app/system/settings/advanced';

export const ADVANCED_GROUPS = ['system', 'security', 'models', 'experiments'] as const;
export type AdvancedGroup = (typeof ADVANCED_GROUPS)[number];

export interface AdvancedSetting {
  id: string;
  group: AdvancedGroup;
  title: string;
  description: string;
  searchPrefixes: string[];
}

/** The former settings tabs keep stable anchors for bookmarks and in-app links. */
export const ADVANCED_SETTINGS: AdvancedSetting[] = [
  { id: 'system', group: 'system', title: 'settings.system', description: 'advancedSettings.system.desc', searchPrefixes: ['settings.restartNote', 'settings.system.bind', 'settings.system.bind.custom', 'settings.system.bind.help', 'settings.system.cooldown', 'settings.system.cooldown.help', 'settings.system.dailyDigest', 'settings.system.dailyDigest.at', 'settings.system.dailyDigest.at.help', 'settings.system.dailyDigest.desc', 'settings.system.dailyDigest.enabled', 'settings.system.dailyDigest.enabled.help', 'settings.system.dangerTitle', 'settings.system.defaultAgent', 'settings.system.defaultAgent.help', 'settings.system.general', 'settings.system.healthInterval', 'settings.system.healthInterval.help', 'settings.system.inferenceMode', 'settings.system.inferenceMode.help', 'settings.system.logFormat', 'settings.system.logFormat.help', 'settings.system.mdns', 'settings.system.mdns.help', 'settings.system.miniapp', 'settings.system.miniapp.enabled', 'settings.system.miniapp.enabled.help', 'settings.system.name', 'settings.system.name.help', 'settings.system.name.restartNote', 'settings.system.rotation', 'settings.system.server', 'settings.system.server.desc', 'settings.system.serverRestart'] },
  { id: 'rotation', group: 'system', title: 'settings.general.rotationStrategy', description: 'settings.general.rotationStrategy.help', searchPrefixes: ['settings.rotation'] },
  { id: 'doctor', group: 'system', title: 'settings.doctor', description: 'settings.doctor.desc', searchPrefixes: ['doctor'] },
  { id: 'billing', group: 'system', title: 'manage.billing', description: 'manage.billing.desc', searchPrefixes: ['billing'] },
  { id: 'configRaw', group: 'system', title: 'settings.configRaw', description: 'settings.configRaw.desc', searchPrefixes: ['configRaw'] },
  { id: 'security', group: 'security', title: 'advancedSettings.security.title', description: 'advancedSettings.security.desc', searchPrefixes: ['settings.sandbox', 'settings.computerUse', 'browserLogin', 'settings.files', 'settings.memory', 'settings.telemetry', 'settings.acp', 'settings.system.remoteAccess', 'settings.system.secrets', 'settings.system.smBackend', 'settings.system.vault', 'settings.system.onepassword', 'settings.system.infisical', 'settings.system.authToken', 'settings.system.authToken', 'settings.system.dangerDesc', 'settings.system.dangerTitle', 'settings.system.gateway', 'settings.system.memory', 'settings.system.noveltyGate', 'settings.system.noveltyGate.help', 'settings.system.port', 'settings.system.port.help', 'settings.system.remoteAccess', 'settings.system.remoteAccess.add', 'settings.system.remoteAccess.builtin', 'settings.system.remoteAccess.desc', 'settings.system.remoteAccess.empty', 'settings.system.remoteAccess.help', 'settings.system.remoteAccess.remove', 'settings.system.secrets', 'settings.system.smBackend', 'settings.system.vaultAddr', 'settings.system.vaultMount', 'settings.system.vaultSection', 'settings.system.vaultToken', 'settings.system.writeOnly'] },
  { id: 'logging', group: 'security', title: 'settings.general.logLevel', description: 'settings.general.logLevel.help', searchPrefixes: ['settings.logLevel'] },
  { id: 'container', group: 'security', title: 'settings.container', description: 'settings.container.desc', searchPrefixes: ['container'] },
  { id: 'delegation', group: 'security', title: 'settings.delegation', description: 'settings.delegation.desc', searchPrefixes: ['delegation'] },
  { id: 'redaction', group: 'security', title: 'settings.redaction', description: 'advancedSettings.redaction.desc', searchPrefixes: ['redaction'] },
  { id: 'browser', group: 'security', title: 'settings.browser', description: 'settings.browser.desc', searchPrefixes: ['browser'] },
  { id: 'utility', group: 'models', title: 'settings.utility.title', description: 'settings.utility.provider.help', searchPrefixes: ['settings.utility'] },
  { id: 'knowledgeSearch', group: 'models', title: 'settings.knowledgeSearch', description: 'settings.knowledgeSearch.desc', searchPrefixes: ['embedding'] },
  { id: 'heartbeat', group: 'experiments', title: 'settings.heartbeat', description: 'settings.heartbeat.desc', searchPrefixes: ['heartbeat'] },
  { id: 'automation', group: 'experiments', title: 'settings.automation.tab', description: 'settings.automation.tab.desc', searchPrefixes: ['settings.automation', 'settings.team', 'settings.tick', 'settings.takeover', 'settings.mail'] },
  { id: 'calibration', group: 'experiments', title: 'settings.calibration', description: 'settings.calibration.desc', searchPrefixes: ['calibration'] },
  { id: 'autopilot', group: 'experiments', title: 'settings.autopilot', description: 'settings.autopilot.desc', searchPrefixes: ['autopilot'] },
  { id: 'skillSynthesis', group: 'experiments', title: 'settings.skillSynthesis', description: 'skillSynthesis.desc', searchPrefixes: ['skillSynthesis'] },
];

export const LEGACY_ADVANCED_TABS = [
  'system', 'container', 'heartbeat', 'automation', 'delegation', 'calibration',
  'autopilot', 'skillSynthesis', 'redaction', 'doctor', 'browser', 'knowledgeSearch', 'configRaw',
] as const;

/** Search is display-only. Include translated field labels/help within a section. */
export function filterAdvancedSettings(query: string, messages: Record<string, unknown>): AdvancedSetting[] {
  const terms = query.trim().normalize('NFKC').toLocaleLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return ADVANCED_SETTINGS;
  return ADVANCED_SETTINGS.filter((entry) => {
    const texts = [messages[entry.title], messages[entry.description],
      ...Object.entries(messages).filter(([key]) => entry.searchPrefixes.some((prefix) =>
        key === prefix || key.startsWith(`${prefix}.`))).map(([, value]) => value),
    ].filter((value): value is string => typeof value === 'string').map((text) => text.normalize('NFKC').toLocaleLowerCase());
    return texts.some((text) => terms.every((term) => text.includes(term)));
  });
}
