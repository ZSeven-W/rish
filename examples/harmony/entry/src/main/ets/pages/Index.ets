import { common } from '@kit.AbilityKit';
import {
  RishDemoResult,
  runRishHarmonyDemo
} from '../demo/RishDemo';

@Entry
@Component
struct Index {
  @State private report: string =
    'Tap Run to plan grep and execute echo + sha256sum.';

  private runDemo(): void {
    try {
      const context: common.UIAbilityContext =
        getContext(this) as common.UIAbilityContext;
      const result: RishDemoResult = runRishHarmonyDemo(context);
      this.report =
        `protocol: ${result.protocol}\n` +
        `grep plan: ${result.grepPlan}\n` +
        `echo: ${result.echoStdout.trim()}\n` +
        `sha256sum: ${result.sha256Stdout.trim()}`;
    } catch (error) {
      const message: string =
        error instanceof Error ? error.message : 'unknown demo failure';
      this.report = `FAILED: ${message}`;
    }
  }

  build() {
    Column({ space: 18 }) {
      Text('Rish HarmonyOS Demo')
        .fontSize(26)
        .fontWeight(FontWeight.Bold)
      Text(this.report)
        .fontSize(15)
        .fontFamily('monospace')
        .width('100%')
      Button('Run')
        .onClick((): void => this.runDemo())
    }
    .width('100%')
    .height('100%')
    .padding(24)
    .justifyContent(FlexAlign.Center)
  }
}
