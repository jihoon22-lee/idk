# Test fixture only. UBI is not the user's RHEL/폐쇄망 environment.
FROM registry.access.redhat.com/ubi8/ubi@sha256:2dcfc23b79d88fdaeefc6ed0cfb144e41dca600bfa73c38ab5d8dca5b05d1b7f AS tcsh-build

# Public UBI repositories do not ship tcsh. Build the fixed upstream source
# inside UBI rather than enabling a foreign/unsigned package repository.
# Upstream download index: https://www.tcsh.org/ -> https://astron.com/pub/tcsh/
RUN dnf -y --setopt=install_weak_deps=False install gcc make ncurses-devel curl tar gzip \
    && dnf clean all
WORKDIR /tmp/tcsh-build
RUN curl --fail --show-error --location --proto '=https' --tlsv1.2 \
        https://astron.com/pub/tcsh/tcsh-6.24.13.tar.gz -o tcsh.tar.gz \
    && echo '1e927d52e9c85d162bf985f24d13c6ccede9beb880d86fec492ed15480a5c71a  tcsh.tar.gz' | sha256sum --check --strict \
    && tar -xzf tcsh.tar.gz \
    && cd tcsh-6.24.13 \
    && ./configure --prefix=/usr/local \
    && make -j2 \
    && install -D -m 755 tcsh /fixture/tcsh \
    && /fixture/tcsh --version

FROM registry.access.redhat.com/ubi8/ubi@sha256:2dcfc23b79d88fdaeefc6ed0cfb144e41dca600bfa73c38ab5d8dca5b05d1b7f
LABEL org.opencontainers.image.description="idk test-only UBI 8.10 fixture; not target field acceptance"
RUN dnf -y --setopt=install_weak_deps=False install \
        git-core less vim-minimal procps-ng ncurses glibc-langpack-en shadow-utils \
    && dnf clean all \
    && groupadd --gid 10001 idktest \
    && useradd --uid 10001 --gid 10001 --create-home --shell /bin/bash idktest
COPY --from=tcsh-build /fixture/tcsh /usr/local/bin/tcsh
RUN ln -s /usr/local/bin/tcsh /usr/local/bin/csh \
    && /usr/local/bin/tcsh --version \
    && git --version \
    && getconf GNU_LIBC_VERSION \
    && rpm -q git-core less vim-minimal procps-ng ncurses glibc
ENV LANG=C.UTF-8
USER 10001:10001
WORKDIR /home/idktest
