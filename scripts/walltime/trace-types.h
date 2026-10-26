// Types from the kernel trace formats, not libc's 64-bit dev_t.
#include <linux/fuse.h>
typedef int pid_t;
typedef unsigned int dev_t;
