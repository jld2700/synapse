#import <UIKit/UIKit.h>

// App delegate that bridges UIKit lifecycle to the Rust entry point
// (synapse_ios_main) exported by libsynapse_app.a.
@interface SynapseAppDelegate : UIResponder <UIApplicationDelegate>
@property (strong, nonatomic) UIWindow *window;
@end
