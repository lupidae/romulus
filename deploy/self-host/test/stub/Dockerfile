# A stand-in for the API image (ghcr.io/lupidae/romulus-api), for a cheap
# dry run of install.sh: `ROMULUS_TEST_STUB=1 install-test.sh` builds it as
# local/romulus-api:stub. It answers /api/health and the CLI commands
# install.sh calls, and touches no database. Never published, never used
# outside that test.
FROM alpine:latest
# httpd serves the health document; curl is compose.yaml's health check.
RUN apk add --no-cache busybox-extras curl
COPY romulus-api /usr/local/bin/
RUN chmod 755 /usr/local/bin/romulus-api
EXPOSE 8080
ENTRYPOINT ["romulus-api"]
CMD ["serve"]
