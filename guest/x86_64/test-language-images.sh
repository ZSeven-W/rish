#!/bin/sh

# Runs real linux/amd64 language toolchains against the diagnostic Docker
# daemon. Images are intentionally configurable because tags move; every run
# prints the resolved content digest before executing code in the container.

set -u

diagnostic_host=${DOCKER_HOST:-tcp://127.0.0.1:12375}
remove_images=${RISH_REMOVE_TEST_IMAGES:-0}
failures=0
passes=0

java_image=${RISH_JAVA_IMAGE:-eclipse-temurin:21-jdk-alpine}
go_image=${RISH_GO_IMAGE:-golang:1.25-alpine}
rust_image=${RISH_RUST_IMAGE:-rust:1-alpine}
node_image=${RISH_NODE_IMAGE:-node:22-alpine}
bun_image=${RISH_BUN_IMAGE:-oven/bun:1-alpine}

docker_guest() {
    docker --host "$diagnostic_host" "$@"
}

test_image() {
    language=$1
    image=$2
    program=$3

    echo "=== $language | $image ==="
    if ! docker_guest pull --quiet --platform linux/amd64 "$image"; then
        echo "RESULT $language FAIL pull"
        failures=$((failures + 1))
        return
    fi

    digest=$(docker_guest image inspect \
        --format '{{index .RepoDigests 0}}' "$image")
    size=$(docker_guest image inspect --format '{{.Size}}' "$image")
    echo "IMAGE $language digest=$digest size_bytes=$size"

    if docker_guest run --rm --network host --platform linux/amd64 \
        "$image" sh -c "$program"; then
        echo "RESULT $language PASS"
        passes=$((passes + 1))
    else
        status=$?
        echo "RESULT $language FAIL exit=$status"
        failures=$((failures + 1))
    fi

    if [ "$remove_images" = 1 ]; then
        docker_guest image rm "$image" >/dev/null 2>&1 || true
    fi
}

if ! docker_guest version --format \
    'SERVER version={{.Server.Version}} os={{.Server.Os}} arch={{.Server.Arch}}'; then
    echo "cannot reach diagnostic Docker daemon at $diagnostic_host" >&2
    exit 2
fi

test_image java "$java_image" '
java -version
printf "%s\n" "public class Hello { public static void main(String[] args) { System.out.println(\"RISH_JAVA_OK \" + System.getProperty(\"os.arch\")); } }" > /tmp/Hello.java
javac /tmp/Hello.java
java -cp /tmp Hello
'

test_image go "$go_image" '
go version
printf "%s\n" "package main" "import \"fmt\"" "func main() { fmt.Println(\"RISH_GO_OK\") }" > /tmp/main.go
GOCACHE=/tmp/go-cache go run /tmp/main.go
'

test_image rust "$rust_image" '
rustc --version
cargo --version
printf "%s\n" "fn main() { println!(\"RISH_RUST_OK\"); }" > /tmp/main.rs
rustc /tmp/main.rs -o /tmp/rish-rust-hello
/tmp/rish-rust-hello
'

test_image nodejs "$node_image" '
node --version
npm --version
node -e "console.log(\"RISH_NODE_OK\", process.arch, process.platform)"
'

test_image bun "$bun_image" '
bun --version
bun -e "console.log(\"RISH_BUN_OK\", process.arch, process.platform)"
'

echo "SUMMARY pass=$passes fail=$failures"
[ "$failures" -eq 0 ]
