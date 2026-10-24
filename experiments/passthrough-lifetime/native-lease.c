#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#define REQUIRE(x) do { if (!(x)) { perror(#x); return 1; } } while (0)
int main(int argc, char **argv) {
    REQUIRE(argc == 2);
    signal(SIGIO, SIG_IGN);
    const char *names[] = {"readonly", "private-writable", "shared-writable"};
    printf("{\"cases\":[");
    for (int i=0;i<3;i++) {
        char path[4096];
        REQUIRE(snprintf(path,sizeof(path),"%s/sl7-native-lease-XXXXXX",argv[1]) < (int)sizeof(path));
        int init=mkstemp(path); REQUIRE(init>=0);
        char bytes[4096]; memset(bytes,'x',sizeof(bytes));
        REQUIRE(write(init,bytes,sizeof(bytes)) == sizeof(bytes)); REQUIRE(close(init)==0);
        int pin=open(path,O_PATH); REQUIRE(pin>=0);
        int lower=open(path,i==2?O_RDWR:O_RDONLY); REQUIRE(lower>=0);
        void *mapping=mmap(NULL,sizeof(bytes),PROT_READ|(i?PROT_WRITE:0),i==1?MAP_PRIVATE:MAP_SHARED,lower,0);
        REQUIRE(mapping!=MAP_FAILED); REQUIRE(close(lower)==0);
        int retirement=open(path,O_RDWR); REQUIRE(retirement>=0);
        errno=0; int result=fcntl(retirement,F_SETLEASE,F_WRLCK); int held_errno=errno;
        if (result==0) REQUIRE(fcntl(retirement,F_SETLEASE,F_UNLCK)==0);
        REQUIRE(*(volatile char *)mapping=='x');
        REQUIRE(munmap(mapping,sizeof(bytes))==0);
        errno=0; int after=fcntl(retirement,F_SETLEASE,F_WRLCK); int after_errno=errno;
        if (after==0) REQUIRE(fcntl(retirement,F_SETLEASE,F_UNLCK)==0);
        printf("%s{\"mapping\":\"%s\",\"blocked_with_live_mapping\":%s,\"held_errno\":%d,\"granted_after_munmap\":%s,\"after_errno\":%d}",i?",":"",names[i],result<0&&held_errno==EAGAIN?"true":"false",held_errno,after==0?"true":"false",after_errno);
        REQUIRE(close(retirement)==0); REQUIRE(close(pin)==0); REQUIRE(unlink(path)==0);
    }
    puts("],\"limitation\":\"Native mapping lifetime probe, not a FUSE passthrough correctness test\"}");
    return 0;
}
