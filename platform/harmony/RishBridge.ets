/**
 * ArkTS adapter for the Rust planner and the portable native-offload protocol.
 *
 * A plan is never permission to execute an arbitrary operation. Dispatch below
 * uses a fixed allow-list and rejects Linux-kernel semantics on the HAP side.
 */
import rish from 'librish_ffi.so';

// ArkTS represents aggregate JSON values as Object; the codec boundary still
// accepts only data produced by JSON.parse/JSON.stringify.
export type RishJSONValue =
  null | boolean | number | string | Object;

/** In-memory representation of `rish_core::GuestCommand`. */
export interface RishGuestCommand {
  program: string;
  args: string[];
  env: Record<string, string>;
  cwd: string;
  /** Kept binary in ArkTS; the codec maps it to Rust's JSON integer array. */
  stdin: Uint8Array;
}

export interface RishCapabilityRequirement {
  capability: string;
  kernelSemanticsRequired: boolean;
}

/** In-memory representation of `rish_core::HostCall`. */
export interface RishHostCall {
  protocolVersion: number;
  id: number;
  operation: string;
  command: RishGuestCommand;
  requirements: RishCapabilityRequirement[];
  payload: RishJSONValue;
}

/** In-memory representation of `rish_core::HostReply`. */
export interface RishHostReply {
  exitCode: number;
  stdout: Uint8Array;
  stderr: Uint8Array;
  payload: RishJSONValue;
}

interface WireGuestCommand {
  program: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
  stdin?: number[];
}

interface WireCapabilityRequirement {
  capability: string;
  kernel_semantics_required?: boolean;
}

interface WireHostCall {
  protocol_version: number;
  id: number;
  operation: string;
  command: WireGuestCommand;
  requirements?: WireCapabilityRequirement[];
  payload?: RishJSONValue;
}

interface WireHostReply {
  exit_code: number;
  stdout: number[];
  stderr: number[];
  payload: RishJSONValue;
}

function emptyBytes(): Uint8Array {
  return new Uint8Array(0);
}

function utf8(text: string): Uint8Array {
  const bytes: number[] = [];
  for (let index: number = 0; index < text.length; index++) {
    let codePoint: number = text.charCodeAt(index);
    if (codePoint >= 0xD800 && codePoint <= 0xDBFF &&
        index + 1 < text.length) {
      const low: number = text.charCodeAt(index + 1);
      if (low >= 0xDC00 && low <= 0xDFFF) {
        codePoint = 0x10000 + ((codePoint - 0xD800) << 10) +
          (low - 0xDC00);
        index++;
      }
    }

    if (codePoint <= 0x7F) {
      bytes.push(codePoint);
    } else if (codePoint <= 0x7FF) {
      bytes.push(0xC0 | (codePoint >> 6));
      bytes.push(0x80 | (codePoint & 0x3F));
    } else if (codePoint <= 0xFFFF) {
      bytes.push(0xE0 | (codePoint >> 12));
      bytes.push(0x80 | ((codePoint >> 6) & 0x3F));
      bytes.push(0x80 | (codePoint & 0x3F));
    } else {
      bytes.push(0xF0 | (codePoint >> 18));
      bytes.push(0x80 | ((codePoint >> 12) & 0x3F));
      bytes.push(0x80 | ((codePoint >> 6) & 0x3F));
      bytes.push(0x80 | (codePoint & 0x3F));
    }
  }
  return new Uint8Array(bytes);
}

function byteArray(value: number[] | undefined, field: string): Uint8Array {
  if (value === undefined) {
    return emptyBytes();
  }
  if (!Array.isArray(value)) {
    throw new Error(`${field} must be an array`);
  }
  for (let index: number = 0; index < value.length; index++) {
    const byte: number = value[index];
    if (!Number.isInteger(byte) || byte < 0 || byte > 255) {
      throw new Error(`${field}[${index}] is not a byte`);
    }
  }
  return new Uint8Array(value);
}

function stringArray(
  value: string[] | undefined,
  field: string
): string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new Error(`${field} must be an array`);
  }
  value.forEach((item: string, index: number) => {
    if (typeof item !== 'string') {
      throw new Error(`${field}[${index}] must be a string`);
    }
  });
  return value;
}

function stringMap(
  value: Record<string, string> | undefined,
  field: string
): Record<string, string> {
  if (value === undefined) {
    return {};
  }
  if (typeof value !== 'object' || value === null ||
      Array.isArray(value)) {
    throw new Error(`${field} must be an object`);
  }
  Object.keys(value).forEach((key: string) => {
    if (typeof value[key] !== 'string') {
      throw new Error(`${field}.${key} must be a string`);
    }
  });
  return value;
}

function bytesForWire(value: Uint8Array): number[] {
  const result: number[] = [];
  for (let index: number = 0; index < value.length; index++) {
    result.push(value[index]);
  }
  return result;
}

/** Strict conversion between Rust snake_case JSON and ArkTS binary models. */
export class RishHostCodec {
  static decodeHostCall(json: string): RishHostCall {
    if (json.length > 8 * 1024 * 1024) {
      throw new Error('host call exceeds the JSON wire size limit');
    }
    const wire: WireHostCall = JSON.parse(json) as WireHostCall;
    if (typeof wire.protocol_version !== 'number' ||
        !Number.isInteger(wire.protocol_version) ||
        wire.protocol_version < 0) {
      throw new Error('protocol_version must be a non-negative integer');
    }
    // JSON numbers cannot losslessly represent all Rust u64 values.
    if (typeof wire.id !== 'number' ||
        !Number.isSafeInteger(wire.id) || wire.id < 0) {
      throw new Error('id must be a non-negative JSON safe integer');
    }
    if (typeof wire.operation !== 'string' || wire.operation.length === 0) {
      throw new Error('operation must be a non-empty string');
    }
    if (wire.command === undefined ||
        typeof wire.command.program !== 'string' ||
        wire.command.program.length === 0) {
      throw new Error('command.program must be a non-empty string');
    }

    if (wire.command.cwd !== undefined &&
        typeof wire.command.cwd !== 'string') {
      throw new Error('command.cwd must be a string');
    }
    const args: string[] =
      stringArray(wire.command.args, 'command.args');
    const env: Record<string, string> =
      stringMap(wire.command.env, 'command.env');
    if (wire.requirements !== undefined &&
        !Array.isArray(wire.requirements)) {
      throw new Error('requirements must be an array');
    }
    const requirements: RishCapabilityRequirement[] =
      (wire.requirements ?? []).map(
        (item: WireCapabilityRequirement): RishCapabilityRequirement => {
          if (item === null || typeof item !== 'object' ||
              typeof item.capability !== 'string' ||
              item.capability.length === 0) {
            throw new Error('requirement capability must be non-empty');
          }
          if (item.kernel_semantics_required !== undefined &&
              typeof item.kernel_semantics_required !== 'boolean') {
            throw new Error(
              'kernel_semantics_required must be a boolean'
            );
          }
          return {
            capability: item.capability,
            kernelSemanticsRequired:
              item.kernel_semantics_required ?? false
          };
        }
      );

    return {
      protocolVersion: wire.protocol_version,
      id: wire.id,
      operation: wire.operation,
      command: {
        program: wire.command.program,
        args: args,
        env: env,
        cwd: wire.command.cwd ?? '/',
        stdin: byteArray(wire.command.stdin, 'command.stdin')
      },
      requirements: requirements,
      payload: wire.payload ?? null
    };
  }

  static encodeHostReply(reply: RishHostReply): string {
    const wire: WireHostReply = {
      exit_code: reply.exitCode,
      stdout: bytesForWire(reply.stdout),
      stderr: bytesForWire(reply.stderr),
      payload: reply.payload
    };
    return JSON.stringify(wire);
  }
}

class RishCancelledError extends Error {
  constructor() {
    super('rish host operation cancelled');
  }
}

/**
 * Cooperative cancellation for Promise-based ArkTS handlers. The Page/Ability
 * owner should cancel this token from its lifecycle cleanup.
 */
export class RishCancellationToken {
  private cancelled: boolean = false;

  cancel(): void {
    this.cancelled = true;
  }

  get isCancelled(): boolean {
    return this.cancelled;
  }

  throwIfCancelled(): void {
    if (this.cancelled) {
      throw new RishCancelledError();
    }
  }
}

/** Promise-based, binary-safe native operation contract. */
export interface RishHostOperationHandler {
  handle(
    call: RishHostCall,
    cancellation: RishCancellationToken
  ): Promise<RishHostReply>;
}

function failure(
  code: string,
  message: string,
  exitCode: number = 125
): RishHostReply {
  return {
    exitCode: exitCode,
    stdout: emptyBytes(),
    stderr: utf8(`${message}\n`),
    payload: {
      error: {
        code: code,
        message: message
      }
    }
  };
}

function basename(program: string): string {
  const separator: number = program.lastIndexOf('/');
  return separator < 0 ? program : program.substring(separator + 1);
}

function isSafeUnitName(unit: string): boolean {
  if (unit.length === 0 || unit.length > 128) {
    return false;
  }
  for (let index: number = 0; index < unit.length; index++) {
    const code: number = unit.charCodeAt(index);
    const allowed: boolean =
      (code >= 48 && code <= 57) ||
      (code >= 65 && code <= 90) ||
      (code >= 97 && code <= 122) ||
      code === 45 || code === 46 || code === 64 || code === 95;
    if (!allowed) {
      return false;
    }
  }
  return true;
}

/**
 * App-local service state only. It is not systemd, PID 1, a cgroup manager, or
 * a launcher for arbitrary Linux service processes.
 */
export class RishInMemoryServiceSupervisor
implements RishHostOperationHandler {
  private static readonly MAXIMUM_UNITS: number = 256;
  private units: Map<string, string> = new Map<string, string>();

  async handle(
    call: RishHostCall,
    cancellation: RishCancellationToken
  ): Promise<RishHostReply> {
    cancellation.throwIfCancelled();
    if (call.operation !== 'service.systemctl' ||
        basename(call.command.program) !== 'systemctl') {
      return failure(
        'command_operation_mismatch',
        'service.systemctl only accepts the systemctl command'
      );
    }
    if (call.command.args.length === 0) {
      return this.usage();
    }

    const verb: string = call.command.args[0];
    if (verb === 'list-units') {
      if (call.command.args.length !== 1) {
        return this.usage();
      }
      const names: string[] = Array.from(this.units.keys()).sort();
      let output: string = '';
      names.forEach((name: string) => {
        output += `${name} ${this.units.get(name) ?? 'inactive'}\n`;
      });
      return this.reply(0, output, verb, null);
    }

    if (verb !== 'start' && verb !== 'stop' && verb !== 'restart' &&
        verb !== 'status' && verb !== 'is-active') {
      return failure(
        'unsupported_systemctl_verb',
        'in-memory supervisor does not implement that systemctl verb',
        2
      );
    }
    if (call.command.args.length !== 2 ||
        !isSafeUnitName(call.command.args[1])) {
      return this.usage();
    }

    const unit: string = call.command.args[1];
    if (verb === 'start' || verb === 'restart') {
      if (!this.units.has(unit) &&
          this.units.size >=
            RishInMemoryServiceSupervisor.MAXIMUM_UNITS) {
        return failure(
          'supervisor_capacity_exceeded',
          'in-memory supervisor unit limit reached'
        );
      }
      this.units.set(unit, 'active');
      return this.reply(0, '', verb, unit);
    }
    if (verb === 'stop') {
      if (this.units.has(unit)) {
        this.units.set(unit, 'inactive');
      }
      return this.reply(0, '', verb, unit);
    }

    const state: string = this.units.get(unit) ?? 'inactive';
    const exitCode: number = state === 'active' ? 0 : 3;
    if (verb === 'is-active') {
      return this.reply(exitCode, `${state}\n`, verb, unit);
    }
    return this.reply(
      exitCode,
      `${unit}: ${state} (rish in-memory supervisor)\n`,
      verb,
      unit
    );
  }

  private reply(
    exitCode: number,
    output: string,
    verb: string,
    unit: string | null
  ): RishHostReply {
    const payload: Record<string, RishJSONValue> = {
      implementation: 'rish.in_memory_supervisor',
      real_systemd: false,
      verb: verb
    };
    if (unit !== null) {
      payload.unit = unit;
    }
    return {
      exitCode: exitCode,
      stdout: utf8(output),
      stderr: emptyBytes(),
      payload: payload
    };
  }

  private usage(): RishHostReply {
    return failure(
      'invalid_systemctl_arguments',
      'usage: systemctl <start|stop|restart|status|is-active> <unit> | list-units',
      2
    );
  }
}

/**
 * Fixed allow-list dispatcher. Only the `container.docker_api` slot can be
 * injected; arbitrary operation names cannot be registered.
 */
export class RishHostDispatcher {
  static readonly PROTOCOL_VERSION: number = 1;

  private supervisor: RishInMemoryServiceSupervisor;
  private dockerApiHandler: RishHostOperationHandler | null;

  constructor(
    dockerApiHandler: RishHostOperationHandler | null = null,
    supervisor: RishInMemoryServiceSupervisor =
      new RishInMemoryServiceSupervisor()
  ) {
    this.dockerApiHandler = dockerApiHandler;
    this.supervisor = supervisor;
  }

  async dispatch(
    call: RishHostCall,
    cancellation: RishCancellationToken = new RishCancellationToken()
  ): Promise<RishHostReply> {
    if (call.protocolVersion !== RishHostDispatcher.PROTOCOL_VERSION) {
      return failure(
        'unsupported_protocol_version',
        `host supports protocol version ${RishHostDispatcher.PROTOCOL_VERSION}`
      );
    }
    if (!this.isWithinInputLimits(call)) {
      return failure(
        'host_call_limits_exceeded',
        'host call exceeds portable dispatcher limits'
      );
    }
    if (call.requirements.some(
      (item: RishCapabilityRequirement): boolean =>
        item.kernelSemanticsRequired
    )) {
      return failure(
        'kernel_semantics_unavailable',
        'portable Harmony offload cannot satisfy real Linux kernel semantics'
      );
    }

    try {
      cancellation.throwIfCancelled();
      let reply: RishHostReply;
      if (call.operation === 'service.systemctl') {
        reply = await this.supervisor.handle(call, cancellation);
      } else if (call.operation === 'container.docker_api') {
        if (basename(call.command.program) !== 'docker') {
          return failure(
            'command_operation_mismatch',
            'container.docker_api only accepts the docker command'
          );
        }
        if (this.dockerApiHandler === null) {
          return failure(
            'docker_api_unavailable',
            'Docker API handler is not configured; no dockerd was started'
          );
        }
        reply = await this.dockerApiHandler.handle(call, cancellation);
      } else {
        return failure(
          'operation_not_allow_listed',
          'native operation is not allow-listed',
          126
        );
      }
      cancellation.throwIfCancelled();
      if (reply.stdout.length > 8 * 1024 * 1024 ||
          reply.stderr.length > 8 * 1024 * 1024) {
        return failure(
          'host_reply_limits_exceeded',
          'native handler output exceeds dispatcher limits'
        );
      }
      return reply;
    } catch (error) {
      if (error instanceof RishCancelledError || cancellation.isCancelled) {
        return failure(
          'cancelled',
          'native operation was cancelled',
          130
        );
      }
      const message: string =
        error instanceof Error ? error.message : 'unknown handler error';
      return failure(
        'handler_failed',
        `native handler failed: ${message}`
      );
    }
  }

  private isWithinInputLimits(call: RishHostCall): boolean {
    if (call.operation.length > 128 ||
        call.command.program.length > 4096 ||
        call.command.args.length > 64 ||
        call.command.stdin.length > 1048576 ||
        call.requirements.length > 64) {
      return false;
    }
    if (call.command.args.some(
      (item: string): boolean => item.length > 4096
    )) {
      return false;
    }
    if (call.requirements.some(
      (item: RishCapabilityRequirement): boolean =>
        item.capability.length > 128
    )) {
      return false;
    }
    const environmentKeys: string[] = Object.keys(call.command.env);
    if (environmentKeys.length > 128) {
      return false;
    }
    return !environmentKeys.some(
      (key: string): boolean =>
        key.length > 1024 || call.command.env[key].length > 65536
    );
  }
}

/** Planning does not invoke a host operation by itself. */
export function planJson(request: string): string {
  return rish.planJson(request);
}

export function protocolVersion(): number {
  return rish.protocolVersion();
}

export async function dispatchHostCall(
  json: string,
  dispatcher: RishHostDispatcher,
  cancellation: RishCancellationToken = new RishCancellationToken()
): Promise<string> {
  const call: RishHostCall = RishHostCodec.decodeHostCall(json);
  const reply: RishHostReply =
    await dispatcher.dispatch(call, cancellation);
  return RishHostCodec.encodeHostReply(reply);
}
