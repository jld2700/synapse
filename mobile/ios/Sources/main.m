// iOS app entry. UIApplicationMain creates the application instance and
// delegates lifecycle events to SynapseAppDelegate, which boots the Rust
// runtime + Slint UI via synapse_ios_main().
#import <UIKit/UIKit.h>
#import "AppDelegate.h"

int main(int argc, char *argv[]) {
    @autoreleasepool {
        return UIApplicationMain(argc, argv, nil,
                                 NSStringFromClass([SynapseAppDelegate class]));
    }
}
