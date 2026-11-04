/** Best-effort local AAC extraction. Failures always retain the original file. */
export type AudioPreparation = { file: File; path: 'audio' | 'original' };
interface AudioFrame {
  numberOfFrames: number; numberOfChannels: number; sampleRate: number; timestamp: number;
  copyTo(data: Float32Array, options: { planeIndex: number; format: 'f32-planar' }): void;
  close(): void;
}
interface EncodedChunk { byteLength: number; copyTo(target: Uint8Array): void }
interface CodecInstance { configure(config: Record<string, unknown>): void; flush(): Promise<void>; close(): void; state: string }
interface Decoder extends CodecInstance { decode(chunk: unknown): void }
interface Encoder extends CodecInstance { encode(frame: AudioFrame): void }
interface CodecConstructor<T> {
  new(options: { output: (data: never) => void; error: (error: Error) => void }): T;
  isConfigSupported(config: Record<string, unknown>): Promise<{ supported?: boolean }>;
}
interface Codecs {
  AudioDecoder: CodecConstructor<Decoder>; AudioEncoder: CodecConstructor<Encoder>;
  AudioData: new(options: Record<string, unknown>) => AudioFrame;
  EncodedAudioChunk: new(options: Record<string, unknown>) => unknown;
}
interface Track { id: number; codec: string; nb_samples: number; audio?: { sample_rate: number; channel_count: number } }
interface Sample { data: Uint8Array; cts: number; duration: number; timescale: number }
interface Demuxer {
  onReady: (info: { audioTracks: Track[] }) => void;
  onError: (error: string) => void;
  onSamples: (id: number, user: unknown, samples: Sample[]) => void;
  appendBuffer(buffer: ArrayBuffer & { fileStart: number }): number;
  setExtractionOptions(id: number, user: unknown, options: { nbSamples: number }): void;
  seek(time: number, rap: boolean): { offset: number };
  releaseUsedSamples(id: number, count: number): void;
  start(): void; stop(): void; flush(): void;
}
export function canExtractAudio(file: Pick<File, 'name'>, scope: object = globalThis): boolean {
  const codecs = scope as Partial<Codecs>;
  return /\.(mp4|mov)$/i.test(file.name) && !!codecs.AudioDecoder && !!codecs.AudioEncoder && !!codecs.AudioData && !!codecs.EncodedAudioChunk;
}
export async function prepareMeetingAudio(file: File, signal?: AbortSignal, extract = extractAac): Promise<AudioPreparation> {
  if (!canExtractAudio(file)) return { file, path: 'original' };
  try {
    const audio = await extract(file, signal);
    return audio.size > 0 && audio.size < file.size ? { file: audio, path: 'audio' } : { file, path: 'original' };
  } catch (error) {
    if (signal?.aborted) throw error;
    return { file, path: 'original' };
  }
}
const SAMPLE_RATES = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
function adts(payload: Uint8Array, frequency: number): Uint8Array {
  const size = payload.length + 7;
  if (size > 8191) throw new Error('AAC frame too large');
  const packet = new Uint8Array(size);
  packet.set([0xff, 0xf1, (1 << 6) | (frequency << 2), (1 << 6) | (size >> 11), (size >> 3) & 255, ((size & 7) << 5) | 31, 0xfc]);
  packet.set(payload, 7); return packet;
}
async function extractAac(source: File, signal?: AbortSignal): Promise<File> {
  // Exact pin: mp4box 2.4.1, BSD-3-Clause. Dynamic import keeps normal uploads small.
  const { createFile } = await import('mp4box');
  const parser = createFile(false) as unknown as Demuxer;
  const codecs = globalThis as unknown as Codecs;
  let track: Track | undefined;
  let failure: Error | undefined;
  let decoder: Decoder | undefined;
  let encoder: Encoder | undefined;
  let processed = 0;
  let previousEnd: number | undefined;
  let outputBytes = 0;
  const samples: Sample[] = [];
  const packets: BlobPart[] = [];
  const check = () => { signal?.throwIfAborted(); if (failure) throw failure; };
  parser.onError = message => { failure = new Error(message); };
  parser.onReady = info => { track = info.audioTracks[0]; };
  parser.onSamples = (_id, _user, items) => {
    samples.push(...items);
    if (samples.length > 4096) failure = new Error('Audio queue limit exceeded');
  };
  const append = async (offset: number) => {
    check();
    const buffer = await source.slice(offset, offset + 1024 * 1024).arrayBuffer() as ArrayBuffer & { fileStart: number };
    buffer.fileStart = offset;
    const next = parser.appendBuffer(buffer);
    check();
    return next > offset ? next : offset + buffer.byteLength;
  };
  try {
    let offset = 0;
    while (!track && offset < source.size) offset = await append(offset);
    const selected = track as Track | undefined;
    if (!selected?.audio || selected.codec !== 'mp4a.40.2' || !Number.isSafeInteger(selected.nb_samples) || selected.nb_samples < 1) throw new Error('Unsupported audio track');
    const rate = selected.audio.sample_rate;
    const channels = selected.audio.channel_count;
    const frequency = SAMPLE_RATES.indexOf(rate);
    if (frequency < 0 || channels < 1 || channels > 6) throw new Error('Unsupported audio layout');
    const decodeConfig = { codec: selected.codec, sampleRate: rate, numberOfChannels: channels,
      description: new Uint8Array([(2 << 3) | (frequency >> 1), ((frequency & 1) << 7) | (channels << 3)]) };
    const encodeConfig = { codec: 'mp4a.40.2', sampleRate: rate, numberOfChannels: 1, bitrate: 48_000, aac: { format: 'aac' } };
    if (!(await codecs.AudioDecoder.isConfigSupported(decodeConfig)).supported || !(await codecs.AudioEncoder.isConfigSupported(encodeConfig)).supported) throw new Error('Codec unavailable');
    encoder = new codecs.AudioEncoder({ error: e => { failure = e; }, output: (chunk: EncodedChunk) => {
      try {
        const payload = new Uint8Array(chunk.byteLength); chunk.copyTo(payload);
        const packet = adts(payload, frequency);
        outputBytes += packet.byteLength;
        if (outputBytes > Math.min(source.size, 512 * 1024 * 1024)) throw new Error('Output limit exceeded');
        packets.push(packet.buffer as ArrayBuffer);
      } catch (error) { failure = error instanceof Error ? error : new Error('Encoding failed'); }
    } });
    encoder.configure(encodeConfig);
    decoder = new codecs.AudioDecoder({ error: e => { failure = e; }, output: (frame: AudioFrame) => {
      try {
        const mono = new Float32Array(frame.numberOfFrames);
        const plane = new Float32Array(frame.numberOfFrames);
        for (let channel = 0; channel < frame.numberOfChannels; channel++) {
          frame.copyTo(plane, { planeIndex: channel, format: 'f32-planar' });
          for (let i = 0; i < mono.length; i++) mono[i] += plane[i] / frame.numberOfChannels;
        }
        const mixed = new codecs.AudioData({ format: 'f32-planar', sampleRate: frame.sampleRate, numberOfFrames: frame.numberOfFrames,
          numberOfChannels: 1, timestamp: frame.timestamp, data: mono });
        try { encoder!.encode(mixed); } finally { mixed.close(); }
      } catch (error) { failure = error instanceof Error ? error : new Error('Decoding failed'); }
      finally { frame.close(); }
    } });
    decoder.configure(decodeConfig);
    parser.setExtractionOptions(selected.id, null, { nbSamples: 32 });
    offset = parser.seek(0, true).offset;
    parser.start();
    const drain = async () => {
      while (samples.length) {
        check();
        const batch = samples.splice(0, 32);
        for (const sample of batch) {
          const timestamp = Math.round(sample.cts * 1_000_000 / sample.timescale);
          const duration = Math.round(sample.duration * 1_000_000 / sample.timescale);
          if (!Number.isSafeInteger(timestamp) || !Number.isSafeInteger(duration) || duration <= 0 || sample.timescale <= 0
            || (previousEnd !== undefined && Math.abs(timestamp - previousEnd) > 100)) {
            throw new Error('Discontinuous audio timeline');
          }
          previousEnd = timestamp + duration;
          decoder!.decode(new codecs.EncodedAudioChunk({ type: 'key', timestamp, duration, data: sample.data }));
        }
        await decoder!.flush(); await encoder!.flush(); check();
        processed += batch.length;
        parser.releaseUsedSamples(selected.id, processed);
      }
    };
    while (offset < source.size && processed < selected.nb_samples) { await drain(); offset = await append(offset); }
    parser.flush(); await drain();
    if (processed !== selected.nb_samples || outputBytes === 0) throw new Error('Incomplete audio');
    return new File(packets, source.name.replace(/\.[^.]+$/, '') + '.aac', { type: 'audio/aac', lastModified: source.lastModified });
  } finally {
    parser.stop();
    if (decoder && decoder.state !== 'closed') decoder.close();
    if (encoder && encoder.state !== 'closed') encoder.close();
  }
}
