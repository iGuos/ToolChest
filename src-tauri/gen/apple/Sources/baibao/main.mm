#import <AVFoundation/AVFoundation.h>
#include "bindings/bindings.h"

// 后台保活：iOS 默认在 App 退到后台/锁屏约 30s 后挂起进程，导致局域网互传 / 请求代理服务端中断。
// 配合 Info.plist 的 UIBackgroundModes=audio，持续播放一段「全 0 静音」音频，让系统判定本 App
// 正在播放音频，从而不挂起进程。MixWithOthers 保证不打断用户正在听的音乐（且内容为静音，听不到）。
static AVAudioEngine *gKeepAliveEngine;
static AVAudioPlayerNode *gKeepAlivePlayer;

static void startSilentKeepAlive() {
    NSError *err = nil;
    AVAudioSession *session = [AVAudioSession sharedInstance];
    [session setCategory:AVAudioSessionCategoryPlayback
             withOptions:AVAudioSessionCategoryOptionMixWithOthers
                   error:&err];
    [session setActive:YES error:&err];

    gKeepAliveEngine = [[AVAudioEngine alloc] init];
    gKeepAlivePlayer = [[AVAudioPlayerNode alloc] init];
    [gKeepAliveEngine attachNode:gKeepAlivePlayer];
    AVAudioFormat *fmt = [gKeepAliveEngine.mainMixerNode outputFormatForBus:0];
    [gKeepAliveEngine connect:gKeepAlivePlayer to:gKeepAliveEngine.mainMixerNode format:fmt];

    // 1 秒静音缓冲，循环播放（samples 全为 0，音量再大也听不到）
    AVAudioFrameCount frames = (AVAudioFrameCount)fmt.sampleRate;
    AVAudioPCMBuffer *buf = [[AVAudioPCMBuffer alloc] initWithPCMFormat:fmt frameCapacity:frames];
    if (buf == nil) return;
    buf.frameLength = frames;

    if ([gKeepAliveEngine startAndReturnError:&err]) {
        [gKeepAlivePlayer scheduleBuffer:buf
                                  atTime:nil
                                 options:AVAudioPlayerNodeBufferLoops
                       completionHandler:nil];
        [gKeepAlivePlayer play];
    }

    // 被来电/闹钟等打断后，结束时重新激活并续播，避免一次中断就彻底失去保活
    [[NSNotificationCenter defaultCenter]
        addObserverForName:AVAudioSessionInterruptionNotification
                    object:session
                     queue:[NSOperationQueue mainQueue]
                usingBlock:^(NSNotification *note) {
                    NSNumber *type = note.userInfo[AVAudioSessionInterruptionTypeKey];
                    if (type.unsignedIntegerValue == AVAudioSessionInterruptionTypeEnded) {
                        NSError *e = nil;
                        [session setActive:YES error:&e];
                        if (![gKeepAliveEngine isRunning]) {
                            [gKeepAliveEngine startAndReturnError:&e];
                        }
                        [gKeepAlivePlayer play];
                    }
                }];
}

int main(int argc, char * argv[]) {
    // 在主运行循环启动后（App 已开始启动）再配置音频会话，时序更稳。
    dispatch_async(dispatch_get_main_queue(), ^{
        startSilentKeepAlive();
    });
    ffi::start_app();
    return 0;
}
