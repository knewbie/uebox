#import <AVFoundation/AVFoundation.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>

enum {
    UEBoxCaptureSelecting = 1,
    UEBoxCaptureRecording = 2,
    UEBoxCaptureFinalized = 3,
    UEBoxCaptureCancelled = 4,
    UEBoxCaptureFailed = 5,
    UEBoxCaptureUnsupported = 6,
    UEBoxCaptureBusy = 7,
};

typedef void (*UEBoxCaptureEvent)(uint64_t recording, int status);

API_AVAILABLE(macos(15.0))
@interface UEBoxCapture : NSObject <SCContentSharingPickerObserver, SCStreamDelegate, SCRecordingOutputDelegate>
@property(nonatomic) uint64_t recording;
@property(nonatomic) UEBoxCaptureEvent event;
@property(nonatomic, copy) NSString *path;
@property(nonatomic) BOOL audio;
@property(nonatomic) BOOL stopping;
@property(nonatomic) BOOL stopReturned;
@property(nonatomic) BOOL started;
@property(nonatomic) int terminalStatus;
@property(nonatomic, strong) SCContentSharingPicker *picker;
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) SCRecordingOutput *output;
- (void)present;
- (void)stop;
@end

static NSObject *currentCapture;

@implementation UEBoxCapture

- (void)present {
    SCContentSharingPickerConfiguration *configuration = [[SCContentSharingPickerConfiguration alloc] init];
    configuration.allowedPickerModes = SCContentSharingPickerModeSingleWindow |
        SCContentSharingPickerModeSingleDisplay;
    configuration.allowsChangingSelectedContent = NO;
    self.picker = SCContentSharingPicker.sharedPicker;
    self.picker.defaultConfiguration = configuration;
    [self.picker addObserver:self];
    self.picker.active = YES;
    self.event(self.recording, UEBoxCaptureSelecting);
    [self.picker present];
}

- (void)complete {
    if (currentCapture != self) return;
    // Keep the callback and its generation until both capture and output have
    // settled, since ScreenCaptureKit can deliver these completions in either order.
    UEBoxCaptureEvent event = self.event;
    const uint64_t recording = self.recording;
    const int status = self.terminalStatus;
    [self.picker removeObserver:self];
    self.picker.active = NO;
    self.picker = nil;
    self.stream = nil;
    self.output = nil;
    currentCapture = nil;
    event(recording, status);
}

- (void)requestStop {
    if (self.stopping) return;
    self.stopping = YES;
    [self.stream stopCaptureWithCompletionHandler:^(NSError * _Nullable error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            if (currentCapture != self) return;
            self.stopReturned = YES;
            if (!self.terminalStatus && (error || !self.started)) {
                self.terminalStatus = self.started ? UEBoxCaptureFailed : UEBoxCaptureCancelled;
            }
            if (self.terminalStatus) {
                [self complete];
            } else {
                dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
                    if (currentCapture == self && !self.terminalStatus) {
                        self.terminalStatus = UEBoxCaptureFailed;
                        [self complete];
                    }
                });
            }
        });
    }];
}

- (void)finish:(int)status {
    if (currentCapture != self) return;
    if (!self.terminalStatus) self.terminalStatus = status;
    if (!self.stream || self.stopReturned) {
        [self complete];
    } else {
        [self requestStop];
    }
}

- (void)stop {
    if (self.stopping || currentCapture != self) return;
    if (!self.stream) {
        [self finish:UEBoxCaptureCancelled];
    } else {
        [self requestStop];
    }
}

- (void)contentSharingPicker:(SCContentSharingPicker *)picker didCancelForStream:(SCStream * _Nullable)stream {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!self.stream) [self finish:UEBoxCaptureCancelled];
    });
}

- (void)contentSharingPickerStartDidFailWithError:(NSError *)error {
    dispatch_async(dispatch_get_main_queue(), ^{ [self finish:UEBoxCaptureFailed]; });
}

- (void)contentSharingPicker:(SCContentSharingPicker *)picker
        didUpdateWithFilter:(SCContentFilter *)filter
                  forStream:(SCStream * _Nullable)stream {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (currentCapture != self || self.stream || stream) return;

        const CGRect rect = filter.contentRect;
        const CGFloat scale = filter.pointPixelScale;
        const CGFloat pixelWidth = rect.size.width * scale;
        const CGFloat pixelHeight = rect.size.height * scale;
        if (!isfinite(pixelWidth) || !isfinite(pixelHeight) || pixelWidth < 2 || pixelHeight < 2) {
            [self finish:UEBoxCaptureFailed];
            return;
        }
        const CGFloat shrink = fmin(1.0, fmin(2560.0 / pixelWidth, 1440.0 / pixelHeight));
        SCStreamConfiguration *settings = [[SCStreamConfiguration alloc] init];
        settings.width = MAX(2, (NSUInteger)(pixelWidth * shrink) & ~((NSUInteger)1));
        settings.height = MAX(2, (NSUInteger)(pixelHeight * shrink) & ~((NSUInteger)1));
        settings.minimumFrameInterval = CMTimeMake(1, 30);
        settings.capturesAudio = self.audio;
        settings.excludesCurrentProcessAudio = YES;
        settings.queueDepth = 3;

        SCRecordingOutputConfiguration *target = [[SCRecordingOutputConfiguration alloc] init];
        target.outputURL = [NSURL fileURLWithPath:self.path];
        target.videoCodecType = AVVideoCodecTypeH264;
        target.outputFileType = AVFileTypeMPEG4;
        self.output = [[SCRecordingOutput alloc] initWithConfiguration:target delegate:self];
        self.stream = [[SCStream alloc] initWithFilter:filter configuration:settings delegate:self];
        NSError *error = nil;
        if (![self.stream addRecordingOutput:self.output error:&error]) {
            self.stream = nil;
            [self finish:UEBoxCaptureFailed];
            return;
        }
        [self.stream startCaptureWithCompletionHandler:^(NSError * _Nullable startError) {
            if (startError) {
                dispatch_async(dispatch_get_main_queue(), ^{
                    self.stream = nil;
                    [self finish:UEBoxCaptureFailed];
                });
            }
        }];
    });
}

- (void)recordingOutputDidStartRecording:(SCRecordingOutput *)recordingOutput {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (currentCapture == self) {
            self.started = YES;
            if (!self.stopping) self.event(self.recording, UEBoxCaptureRecording);
        }
    });
}

- (void)recordingOutput:(SCRecordingOutput *)recordingOutput didFailWithError:(NSError *)error {
    dispatch_async(dispatch_get_main_queue(), ^{ [self finish:UEBoxCaptureFailed]; });
}

- (void)recordingOutputDidFinishRecording:(SCRecordingOutput *)recordingOutput {
    dispatch_async(dispatch_get_main_queue(), ^{ [self finish:UEBoxCaptureFinalized]; });
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    dispatch_async(dispatch_get_main_queue(), ^{
        self.stream = nil;
        [self finish:UEBoxCaptureFailed];
    });
}
@end

bool uebox_capture_available(void) {
    if (@available(macOS 15.0, *)) return true;
    return false;
}

void uebox_capture_start(const char *path, bool audio, uint64_t recording, UEBoxCaptureEvent event) {
    NSString *destination = [NSString stringWithUTF8String:path];
    dispatch_async(dispatch_get_main_queue(), ^{
        if (@available(macOS 15.0, *)) {
            if (currentCapture) {
                event(recording, UEBoxCaptureBusy);
                return;
            }
            if (!destination) {
                event(recording, UEBoxCaptureFailed);
                return;
            }
            UEBoxCapture *capture = [[UEBoxCapture alloc] init];
            capture.recording = recording;
            capture.path = destination;
            capture.audio = audio;
            capture.event = event;
            currentCapture = capture;
            [capture present];
        } else {
            event(recording, UEBoxCaptureUnsupported);
        }
    });
}

void uebox_capture_stop(uint64_t recording) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (@available(macOS 15.0, *)) {
            UEBoxCapture *capture = (UEBoxCapture *)currentCapture;
            if (capture.recording == recording) [capture stop];
        }
    });
}
