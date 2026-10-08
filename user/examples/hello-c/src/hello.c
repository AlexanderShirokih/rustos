#include <stdio.h>

int main(int argc, char **argv)
{
    printf("hello from %s\n", argc > 0 ? argv[0] : "C");
    return 0;
}
