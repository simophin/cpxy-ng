/* Android shell-UID probe: app instrumentation itself is excluded from VPN.
 * Build with the NDK clang, push to /data/local/tmp, and use SplitRoutingTest. */
#include <arpa/inet.h>
#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 4) return 2;
    int udp = !strcmp(argv[1], "dns");
    struct addrinfo hints = {.ai_family = AF_INET, .ai_socktype = udp ? SOCK_DGRAM : SOCK_STREAM};
    struct addrinfo *address = NULL;
    if (getaddrinfo(argv[2], argv[3], &hints, &address)) return 3;
    int fd = socket(address->ai_family, address->ai_socktype, 0);
    struct timeval timeout = {.tv_sec = 15};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    if (connect(fd, address->ai_addr, address->ai_addrlen)) return 4;
    freeaddrinfo(address);
    char data[8192];
    if (udp) {
        const unsigned char query[] = {0x43,0x50,1,0,0,1,0,0,0,0,0,0,7,'e','x','a','m','p','l','e',3,'c','o','m',0,0,1,0,1};
        if (send(fd, query, sizeof(query), 0) != sizeof(query)) return 5;
        int n = recv(fd, data, sizeof(data), 0);
        if (n < 12 || (unsigned char)data[0] != 0x43 || (unsigned char)data[1] != 0x50 || !(data[2] & 0x80) || (data[3] & 15) || !(data[6] || data[7])) return 6;
    } else {
        int size = snprintf(data, sizeof(data), "GET / HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", argv[2]);
        if (send(fd, data, size, 0) != size) return 7;
        int n = recv(fd, data, sizeof(data), 0);
        if (n < 12 || memcmp(data, "HTTP/1.", 7)) return 8;
    }
    close(fd);
    puts("OK");
    return 0;
}
