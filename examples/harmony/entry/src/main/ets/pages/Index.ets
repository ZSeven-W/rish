import { common } from '@kit.AbilityKit';
import {
  RishDemoResult,
  runRishHarmonyDemo
} from '../demo/RishDemo';

@Entry
@Component
struct Index {
  @State private phase: string = 'STARTING';
  @State private protocol: string = '1';
  @State private grepPlan: string = 'Waiting for planner';
  @State private echoOutput: string = 'Waiting for applet';
  @State private checksumOutput: string = 'Waiting for applet';
  @State private failure: string = '';

  aboutToAppear(): void {
    this.runDemo();
  }

  private runDemo(): void {
    this.phase = 'RUNNING';
    this.failure = '';
    try {
      const context: common.UIAbilityContext =
        getContext(this) as common.UIAbilityContext;
      const result: RishDemoResult = runRishHarmonyDemo(context);
      this.protocol = result.protocol.toString();
      this.grepPlan = result.grepPlan;
      this.echoOutput = result.echoStdout.trim();
      this.checksumOutput = result.sha256Stdout.trim();
      this.phase = 'PASS';
    } catch (error) {
      const message: string =
        error instanceof Error ? error.message : 'unknown demo failure';
      this.grepPlan = 'Planner did not complete';
      this.echoOutput = 'Applet did not complete';
      this.checksumOutput = 'Applet did not complete';
      this.failure = message;
      this.phase = 'FAILED';
    }
  }

  build() {
    Scroll() {
      Column() {
        Column() {
          Row({ space: 12 }) {
            Stack() {
              Text('>_')
                .fontSize(18)
                .fontWeight(FontWeight.Bold)
                .fontColor('#07111F')
                .fontFamily('monospace')
            }
            .width(44)
            .height(44)
            .borderRadius(13)
            .backgroundColor('#5EEAD4')
            .alignContent(Alignment.Center)

            Column({ space: 2 }) {
              Text('RISH')
                .fontSize(18)
                .fontWeight(FontWeight.Bold)
                .fontColor('#FFFFFF')
                .letterSpacing(2.2)
              Text('PORTABLE COMMAND RUNTIME')
                .fontSize(10)
                .fontWeight(FontWeight.Medium)
                .fontColor('#94A3B8')
                .letterSpacing(1.1)
            }
            .alignItems(HorizontalAlign.Start)
          }
          .width('100%')

          Row({ space: 8 }) {
            Text('HARMONYOS')
              .fontSize(10)
              .fontWeight(FontWeight.Bold)
              .fontColor('#5EEAD4')
              .letterSpacing(0.8)
              .padding({
                left: 11,
                right: 11,
                top: 7,
                bottom: 7
              })
              .backgroundColor('#102436')
              .borderRadius(99)
              .border({ width: 1, color: '#1D3A4E' })
            Text('ARM64')
              .fontSize(10)
              .fontWeight(FontWeight.Bold)
              .fontColor('#FFFFFF')
              .letterSpacing(0.8)
              .padding({
                left: 11,
                right: 11,
                top: 7,
                bottom: 7
              })
              .backgroundColor('#101C2D')
              .borderRadius(99)
              .border({ width: 1, color: '#26364B' })
            Text(`ABI v${this.protocol}`)
              .fontSize(10)
              .fontWeight(FontWeight.Bold)
              .fontColor('#7EE787')
              .letterSpacing(0.8)
              .padding({
                left: 11,
                right: 11,
                top: 7,
                bottom: 7
              })
              .backgroundColor('#132A2D')
              .borderRadius(99)
              .border({ width: 1, color: '#22503E' })
          }
          .width('100%')
          .margin({ top: 24 })

          Text('Portable commands.\nOne safe runtime.')
            .fontSize(38)
            .fontWeight(FontWeight.Bold)
            .fontColor('#FFFFFF')
            .lineHeight(45)
            .letterSpacing(-0.8)
            .width('100%')
            .margin({ top: 34 })

          Text(
            'Rust applets cross the native bridge with bounded I/O, ' +
            'while Linux kernel privileges stay outside the app sandbox.'
          )
            .fontSize(16)
            .fontColor('#94A3B8')
            .lineHeight(24)
            .width('100%')
            .margin({ top: 14 })

          Column() {
            Row() {
              Row({ space: 6 }) {
                Text('●')
                  .fontSize(9)
                  .fontColor('#FB7185')
                Text('●')
                  .fontSize(9)
                  .fontColor('#FBBF24')
                Text('●')
                  .fontSize(9)
                  .fontColor('#7EE787')
              }
              Blank()
              Text('RISH SESSION')
                .fontSize(10)
                .fontWeight(FontWeight.Bold)
                .fontColor('#94A3B8')
                .letterSpacing(1.2)
              Text(this.phase)
                .fontSize(10)
                .fontWeight(FontWeight.Bold)
                .fontColor(
                  this.phase === 'PASS' ? '#7EE787' :
                    this.phase === 'FAILED' ? '#FB7185' : '#5EEAD4'
                )
                .letterSpacing(0.8)
                .margin({ left: 12 })
            }
            .width('100%')
            .padding({
              left: 18,
              right: 18,
              top: 14,
              bottom: 14
            })
            .border({
              width: { bottom: 1 },
              color: '#203047'
            })

            Column({ space: 9 }) {
              Text('$ rish plan harmony grep -F needle')
                .fontSize(13)
                .fontColor('#5EEAD4')
                .fontFamily('monospace')
                .width('100%')
              Text(`  → ${this.grepPlan}`)
                .fontSize(13)
                .fontColor('#7EE787')
                .fontFamily('monospace')
                .width('100%')

              Text('$ echo hello from HarmonyOS')
                .fontSize(13)
                .fontColor('#5EEAD4')
                .fontFamily('monospace')
                .width('100%')
                .margin({ top: 8 })
              Text(`  ${this.echoOutput}`)
                .fontSize(13)
                .fontColor('#FFFFFF')
                .fontFamily('monospace')
                .width('100%')

              Text('$ printf ··· | sha256sum')
                .fontSize(13)
                .fontColor('#5EEAD4')
                .fontFamily('monospace')
                .width('100%')
                .margin({ top: 8 })
              Text(`  ${this.checksumOutput}`)
                .fontSize(12)
                .fontColor('#FFFFFF')
                .fontFamily('monospace')
                .lineHeight(19)
                .wordBreak(WordBreak.BREAK_ALL)
                .width('100%')

              if (this.failure.length > 0) {
                Text(`error: ${this.failure}`)
                  .fontSize(12)
                  .fontColor('#FB7185')
                  .fontFamily('monospace')
                  .lineHeight(19)
                  .wordBreak(WordBreak.BREAK_ALL)
                  .width('100%')
                  .margin({ top: 8 })
              }
            }
            .width('100%')
            .alignItems(HorizontalAlign.Start)
            .padding(18)
          }
          .width('100%')
          .backgroundColor('#101C2D')
          .borderRadius(22)
          .border({ width: 1, color: '#203047' })
          .margin({ top: 30 })

          Button('Run again')
            .width('100%')
            .height(54)
            .fontSize(16)
            .fontWeight(FontWeight.Bold)
            .fontColor('#07111F')
            .backgroundColor('#5EEAD4')
            .borderRadius(15)
            .margin({ top: 18 })
            .onClick((): void => this.runDemo())

          Text('Runs the same fixed, bounded commands through ABI v1.')
            .fontSize(12)
            .fontColor('#94A3B8')
            .textAlign(TextAlign.Center)
            .width('100%')
            .margin({ top: 11 })

          Row({ space: 9 }) {
            Text('●')
              .fontSize(8)
              .fontColor('#7EE787')
            Text('Portable applets · App sandbox')
              .fontSize(12)
              .fontWeight(FontWeight.Medium)
              .fontColor('#94A3B8')
              .letterSpacing(0.3)
          }
          .justifyContent(FlexAlign.Center)
          .width('100%')
          .margin({ top: 34, bottom: 8 })
        }
        .width('100%')
        .constraintSize({ maxWidth: 720 })
        .padding({
          left: 24,
          right: 24,
          top: 30,
          bottom: 34
        })
      }
      .width('100%')
      .alignItems(HorizontalAlign.Center)
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#07111F')
  }
}
