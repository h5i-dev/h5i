#!/usr/bin/env bash
# Start PostgreSQL 17 in Docker that accepts only TLS connections, for
# crates/h5i-app-pg/src/tls_test.rs. Writes a throwaway CA and a server
# certificate for `localhost` into DIR (default: a new temporary directory)
# and prints the two variables the tests read, e.g.
#   scripts/app/pg-tls.sh "$RUNNER_TEMP/pg-tls" >> "$GITHUB_ENV"
# Everything else goes to stderr. Remove with `docker rm -f h5i-app-pg-tls`.
set -euo pipefail
dir=${1:-$(mktemp -d)}
port=${H5I_APP_TLS_PG_PORT:-55433}
name=${H5I_APP_TLS_PG_NAME:-h5i-app-pg-tls}
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
cd "$dir"

{
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 7 \
    -subj "/CN=h5i-app test CA" -keyout ca.key -out ca.pem \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign"
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -subj "/CN=localhost" -keyout server.key -out server.csr
  printf 'subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n' > server.ext
  openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 7 \
    -extfile server.ext -out server.crt
  chmod 644 server.key server.crt ca.pem
  # Unix-socket trust for the image's init scripts; TCP only over TLS.
  printf 'local all all trust\nhostssl all all all scram-sha-256\nhostnossl all all all reject\n' > pg_hba.conf

  docker rm -f "$name" >/dev/null 2>&1 || true
  # PostgreSQL wants the key owned by its user with mode 0600.
  docker run -d --name "$name" -p "127.0.0.1:$port:5432" \
    -e POSTGRES_USER=h5i -e POSTGRES_PASSWORD=h5i -e POSTGRES_DB=h5i \
    -v "$dir":/certs:ro --entrypoint bash postgres:17 -c \
    'install -o postgres -g postgres -m 600 /certs/server.key /certs/server.crt /tmp/ &&
     exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tmp/server.crt \
       -c ssl_key_file=/tmp/server.key -c hba_file=/certs/pg_hba.conf'
  for _ in $(seq 60); do
    if docker exec "$name" pg_isready -q -U h5i -h 127.0.0.1; then break; fi
    sleep 1
  done
  docker exec "$name" pg_isready -U h5i -h 127.0.0.1
} >&2

echo "H5I_APP_TEST_TLS_DATABASE_URL=postgres://h5i:h5i@localhost:$port/h5i"
echo "H5I_APP_TEST_TLS_CA=$dir/ca.pem"
