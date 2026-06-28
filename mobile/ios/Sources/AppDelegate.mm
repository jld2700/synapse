#import "AppDelegate.h"

// Rust entry point exported from crates/app/src/lib.rs (#[no_mangle]).
extern "C" void synapse_ios_main(void);

@implementation SynapseAppDelegate

- (BOOL)application:(UIApplication *)application
    didFinishLaunchingWithOptions:(NSDictionary *)launchOptions {
    // Boot the Rust runtime + Slint UI. synapse_ios_main blocks on the tokio
    // runtime which drives run_app(); Slint's winit backend attaches its view
    // hierarchy to our window via UIKit's main run loop.
    self.window = [[UIWindow alloc] initWithFrame:[[UIScreen mainScreen] bounds]];
    [self.window makeKeyAndVisible];
    synapse_ios_main();
    return YES;
}

- (void)applicationWillResignActive:(UIApplication *)application {}
- (void)applicationDidEnterBackground:(UIApplication *)application {}
- (void)applicationWillEnterForeground:(UIApplication *)application {}
- (void)applicationDidBecomeActive:(UIApplication *)application {}
- (void)applicationWillTerminate:(UIApplication *)application {}

@end
