import { common } from '@kit.AbilityKit';
import {
  RishAppletConfiguration,
  RishGuestCommand,
  executePortableApplet,
  planJson,
  protocolVersion
} from '../rish/RishBridge';

interface WirePlan {
  kind: string;
  name?: string;
}

interface WirePlanResponse {
  protocol_version: number;
  ok: boolean;
  plan?: WirePlan;
  error?: string;
}

interface WireExecutionPath {
  kind: string;
  name?: string;
}

interface WireExecutionOutcome {
  exit_code: number;
  stdout: number[];
  stderr: number[];
  path: WireExecutionPath;
}

interface WireExecuteResponse {
  protocol_version: number;
  ok: boolean;
  outcome?: WireExecutionOutcome;
  error?: string;
}

export interface RishDemoResult {
  protocol: number;
  grepPlan: string;
  echoStdout: string;
  sha256Stdout: string;
}

function asciiBytes(value: string): Uint8Array {
  const output: number[] = [];
  for (let index: number = 0; index < value.length; index++) {
    const code: number = value.charCodeAt(index);
    if (code > 0x7F) {
      throw new Error('demo input must be ASCII');
    }
    output.push(code);
  }
  return new Uint8Array(output);
}

function asciiText(value: number[], field: string): string {
  let output: string = '';
  for (let index: number = 0; index < value.length; index++) {
    const byte: number = value[index];
    if (!Number.isInteger(byte) || byte < 0 || byte > 0x7F) {
      throw new Error(`${field}[${index}] is not an ASCII byte`);
    }
    output += String.fromCharCode(byte);
  }
  return output;
}

function execute(
  command: RishGuestCommand,
  configuration: RishAppletConfiguration,
  expectedName: string
): WireExecutionOutcome {
  const decoded: WireExecuteResponse =
    JSON.parse(
      executePortableApplet(command, configuration)
    ) as WireExecuteResponse;
  if (decoded.protocol_version !== protocolVersion() ||
      decoded.ok !== true ||
      decoded.outcome === undefined) {
    throw new Error(decoded.error ?? `${expectedName} execution failed`);
  }
  if (decoded.outcome.exit_code !== 0 ||
      decoded.outcome.path.kind !== 'portable_applet' ||
      decoded.outcome.path.name !== expectedName) {
    throw new Error(`${expectedName} returned an unexpected outcome`);
  }
  return decoded.outcome;
}

export function runRishHarmonyDemo(
  context: common.Context
): RishDemoResult {
  const version: number = protocolVersion();
  const grepResponse: WirePlanResponse = JSON.parse(planJson(JSON.stringify({
    platform: 'harmony',
    privilege: 'app_sandbox',
    command: {
      program: 'grep',
      args: ['-F', 'needle'],
      env: {},
      cwd: '/',
      stdin: Array.from(asciiBytes('needle\n'))
    }
  }))) as WirePlanResponse;
  if (grepResponse.protocol_version !== version ||
      grepResponse.ok !== true ||
      grepResponse.plan === undefined ||
      grepResponse.plan.kind !== 'portable_applet' ||
      grepResponse.plan.name !== 'grep') {
    throw new Error(grepResponse.error ?? 'grep was not planned as an applet');
  }

  const configuration: RishAppletConfiguration =
    RishAppletConfiguration.inAppFiles(context);
  const echo: WireExecutionOutcome = execute(
    {
      program: 'echo',
      args: ['hello', 'from', 'HarmonyOS'],
      env: {},
      cwd: '/',
      stdin: new Uint8Array(0)
    },
    configuration,
    'echo'
  );
  const checksum: WireExecutionOutcome = execute(
    {
      program: 'sha256sum',
      args: [],
      env: {},
      cwd: '/',
      stdin: asciiBytes('hello from HarmonyOS\n')
    },
    configuration,
    'sha256sum'
  );
  const echoStdout: string = asciiText(echo.stdout, 'echo.stdout');
  const sha256Stdout: string =
    asciiText(checksum.stdout, 'sha256sum.stdout');
  if (echoStdout !== 'hello from HarmonyOS\n' ||
      sha256Stdout !==
        '455788b5b41c3bcbb2cd5ef572079294a44a8ed1b7b74c17ec0680cd2bf97f27  -\n') {
    throw new Error('portable applet output did not match the demo vectors');
  }

  return {
    protocol: version,
    grepPlan: 'portable_applet:grep',
    echoStdout: echoStdout,
    sha256Stdout: sha256Stdout
  };
}
