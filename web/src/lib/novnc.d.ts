// Minimal typing for the parts of noVNC's RFB client the computer live view
// uses (`@novnc/novnc` ships no type declarations).
declare module '@novnc/novnc' {
  export interface RfbOptions {
    credentials?: { password?: string; username?: string; target?: string };
    shared?: boolean;
    wsProtocols?: string[];
  }
  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, urlOrChannel: string | WebSocket, options?: RfbOptions);
    viewOnly: boolean;
    scaleViewport: boolean;
    resizeSession: boolean;
    clipViewport: boolean;
    focusOnClick: boolean;
    showDotCursor: boolean;
    background: string;
    qualityLevel: number;
    compressionLevel: number;
    disconnect(): void;
    sendCredentials(credentials: { password?: string; username?: string; target?: string }): void;
    focus(): void;
    blur(): void;
  }
}
