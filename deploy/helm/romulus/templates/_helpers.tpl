{{/* Names ----------------------------------------------------------------- */}}

{{- define "romulus.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* <release>, or <release>-romulus when the release name does not say it. */}}
{{- define "romulus.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 40 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 40 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "romulus.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "romulus.version" -}}
{{- .Values.image.tag | default .Chart.AppVersion -}}
{{- end -}}

{{- define "romulus.labels" -}}
helm.sh/chart: {{ include "romulus.chart" . }}
app.kubernetes.io/name: {{ include "romulus.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ include "romulus.version" . | trunc 63 | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: romulus
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{/* romulus.selectorLabels (dict "root" $ "component" "api") */}}
{{- define "romulus.selectorLabels" -}}
app.kubernetes.io/name: {{ include "romulus.name" .root }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{/* A pod template's: no version or chart, so a release changes only the
     pods whose spec it changes (the app database stays up). */}}
{{- define "romulus.podLabels" -}}
{{ include "romulus.selectorLabels" . }}
app.kubernetes.io/part-of: romulus
{{- with .root.Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{- define "romulus.componentLabels" -}}
{{ include "romulus.labels" .root }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "romulus.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- .Values.serviceAccount.name | default (include "romulus.fullname" .) -}}
{{- else -}}
{{- .Values.serviceAccount.name | default "default" -}}
{{- end -}}
{{- end -}}

{{- define "romulus.secretName" -}}
{{- .Values.existingSecret | default (include "romulus.fullname" .) -}}
{{- end -}}

{{/* The Secret holding what values hand the chart (SMTP and OIDC secrets,
     the external database's URL). */}}
{{- define "romulus.envSecretName" -}}
{{- printf "%s-env" (include "romulus.fullname" .) -}}
{{- end -}}

{{- define "romulus.postgresqlName" -}}
{{- printf "%s-postgresql" (include "romulus.fullname" .) -}}
{{- end -}}

{{/* The sandbox of a major: <fullname>-sandbox (17), <fullname>-sandbox-15… */}}
{{- define "romulus.sandboxName" -}}
{{- if eq (toString .major) "17" -}}
{{- printf "%s-sandbox" (include "romulus.fullname" .root) -}}
{{- else -}}
{{- printf "%s-sandbox-%v" (include "romulus.fullname" .root) .major -}}
{{- end -}}
{{- end -}}

{{/* 17, then the extra majors in order. */}}
{{- define "romulus.majors" -}}
{{- $majors := list "17" -}}
{{- range (.Values.sandbox.extraMajors | default list | sortAlpha) -}}
{{- $majors = append $majors (toString .) -}}
{{- end -}}
{{- join "," $majors -}}
{{- end -}}

{{- define "romulus.backupClaim" -}}
{{- .Values.backup.persistence.existingClaim | default (printf "%s-backups" (include "romulus.fullname" .)) -}}
{{- end -}}

{{/* Images ---------------------------------------------------------------- */}}

{{- define "romulus.apiImage" -}}
{{- printf "%s/romulus-api:%s" .Values.image.registry (include "romulus.version" .) -}}
{{- end -}}

{{/* romulus.sandboxImage (dict "root" $ "major" "15") */}}
{{- define "romulus.sandboxImage" -}}
{{- $root := .root -}}
{{- $tag := include "romulus.version" $root -}}
{{- if ne (toString .major) "17" -}}
{{- $tag = printf "%s-pg%v" $tag .major -}}
{{- end -}}
{{- printf "%s/romulus-sandbox:%s" $root.Values.image.registry $tag -}}
{{- end -}}

{{- define "romulus.postgresqlImage" -}}
{{- printf "%s:%s" .Values.postgresql.image.repository .Values.postgresql.image.tag -}}
{{- end -}}

{{- define "romulus.backupImage" -}}
{{- .Values.backup.image | default (include "romulus.postgresqlImage" .) -}}
{{- end -}}

{{/* The URL ---------------------------------------------------------------- */}}

{{- define "romulus.publicUrl" -}}
{{- if .Values.publicUrl -}}
{{- .Values.publicUrl | trimSuffix "/" -}}
{{- else -}}
{{- printf "https://%s" .Values.host -}}
{{- end -}}
{{- end -}}

{{/* Pods --------------------------------------------------------------------- */}}

{{/* Pod Security "restricted": nobody is root, no privilege escalation, no
     capabilities, the runtime's seccomp profile, and a read-only root (the
     writable paths are emptyDirs). romulus.podSecurity (dict "uid" 10001) */}}
{{- define "romulus.podSecurity" -}}
runAsNonRoot: true
runAsUser: {{ .uid }}
runAsGroup: {{ .uid }}
fsGroup: {{ .uid }}
fsGroupChangePolicy: OnRootMismatch
seccompProfile:
  type: RuntimeDefault
{{- end -}}

{{- define "romulus.containerSecurity" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
privileged: false
capabilities:
  drop: [ALL]
{{- end -}}

{{/* The app database's URL, as DATABASE_URL: env entries ending with it. The
     password comes from a Secret and is spliced in by the kubelet
     ($(VAR) expansion), so it never sits in the pod spec. */}}
{{- define "romulus.databaseEnv" -}}
{{- $ext := .Values.postgresql.external -}}
{{- if not $ext.enabled }}
- name: ROMULUS_DATABASE_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ include "romulus.secretName" . }}
      key: postgres-password
- name: DATABASE_URL
  value: {{ printf "postgres://romulus:$(ROMULUS_DATABASE_PASSWORD)@%s:5432/romulus" (include "romulus.postgresqlName" .) | quote }}
{{- else if and $ext.existingSecret $ext.existingSecretUrlKey }}
- name: DATABASE_URL
  valueFrom:
    secretKeyRef:
      name: {{ $ext.existingSecret }}
      key: {{ $ext.existingSecretUrlKey }}
{{- else if $ext.existingSecret }}
- name: ROMULUS_DATABASE_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ $ext.existingSecret }}
      key: {{ $ext.existingSecretPasswordKey }}
- name: DATABASE_URL
  value: {{ printf "postgres://%s:$(ROMULUS_DATABASE_PASSWORD)@%s:%v/%s?sslmode=%s" (include "romulus.urlEncode" $ext.user) (include "romulus.urlHost" $ext.host) $ext.port (include "romulus.urlEncode" $ext.database) $ext.sslmode | quote }}
{{- else }}
- name: DATABASE_URL
  valueFrom:
    secretKeyRef:
      name: {{ include "romulus.envSecretName" . }}
      key: database-url
{{- end }}
{{- end -}}

{{/* Percent-encoding for a URL's user, password or path: urlquery, but a
     space as %20 (a literal + is already %2B). */}}
{{- define "romulus.urlEncode" -}}
{{- . | toString | urlquery | replace "+" "%20" -}}
{{- end -}}

{{/* An IPv6 address goes in brackets. */}}
{{- define "romulus.urlHost" -}}
{{- if and (contains ":" .) (not (hasPrefix "[" .)) -}}
[{{ . }}]
{{- else -}}
{{ . }}
{{- end -}}
{{- end -}}

{{- define "romulus.externalDatabaseUrl" -}}
{{- $ext := .Values.postgresql.external -}}
{{- printf "postgres://%s:%s@%s:%v/%s?sslmode=%s" (include "romulus.urlEncode" $ext.user) (include "romulus.urlEncode" $ext.password) (include "romulus.urlHost" $ext.host) $ext.port (include "romulus.urlEncode" $ext.database) $ext.sslmode -}}
{{- end -}}

{{/* The <release>-env Secret's data: deterministic, so the API's checksum
     of it changes only with what values hand it. */}}
{{- define "romulus.envSecretData" -}}
{{- $ext := .Values.postgresql.external -}}
{{- if and .Values.smtp.password (not .Values.smtp.existingSecret) }}
smtp-password: {{ .Values.smtp.password | toString | b64enc }}
{{- end }}
{{- if and .Values.auth.oidc.clientSecret (not .Values.auth.oidc.existingSecret) }}
oidc-client-secret: {{ .Values.auth.oidc.clientSecret | toString | b64enc }}
{{- end }}
{{- if and $ext.enabled (not $ext.existingSecret) }}
database-url: {{ include "romulus.externalDatabaseUrl" . | b64enc }}
{{- end }}
{{- end -}}

{{/* Whether the chart's <release>-env Secret has anything to hold. */}}
{{- define "romulus.hasEnvSecret" -}}
{{- $ext := .Values.postgresql.external -}}
{{- if or (and .Values.smtp.password (not .Values.smtp.existingSecret)) (and .Values.auth.oidc.clientSecret (not .Values.auth.oidc.existingSecret)) (and $ext.enabled (not $ext.existingSecret)) -}}
true
{{- end -}}
{{- end -}}

{{/* A pod that reaches the app database waits for it to accept connections
     first (compose's depends_on: service_healthy): without it, the API's
     first start on a fresh install would fail and back off. Uses bash's
     /dev/tcp in the API image, reading the host from DATABASE_URL. */}}
{{- define "romulus.waitForDatabase" -}}
- name: wait-for-database
  image: {{ include "romulus.apiImage" . }}
  imagePullPolicy: {{ .Values.image.pullPolicy }}
  command:
    - bash
    - -c
    - |
      url=${DATABASE_URL#*://}
      url=${url##*@}
      hostport=${url%%[/?]*}
      case $hostport in
      \[*\]:*) host=${hostport%]:*}; host=${host#[}; port=${hostport##*]:} ;;
      \[*\]) host=${hostport#[}; host=${host%]}; port=5432 ;;
      *:*) host=${hostport%:*}; port=${hostport##*:} ;;
      *) host=$hostport; port=5432 ;;
      esac
      [ -n "$host" ] || exit 0
      waited=0
      until (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; do
        if [ $((waited % 30)) -eq 0 ]; then echo "waiting for the database at $host:$port"; fi
        sleep 2
        waited=$((waited + 2))
      done
      echo "the database at $host:$port accepts connections"
  env:
    {{- include "romulus.databaseEnv" . | trim | nindent 4 }}
  securityContext:
    {{- include "romulus.containerSecurity" . | nindent 4 }}
  resources:
    requests:
      cpu: 10m
      memory: 16Mi
    limits:
      memory: 64Mi
{{- end -}}

{{/* Checks ------------------------------------------------------------------ */}}

{{/* Passwords romulus's development stacks use (docker-compose.yml): never
     in an install. */}}
{{- define "romulus.devPasswords" -}}
romulus,sandbox_admin,postgres,RomulusDev1!
{{- end -}}

{{/* romulus.checkPassword (dict "value" "…" "what" "existingSecret's …" "min" 16):
     never a development password, at least `min` characters. */}}
{{- define "romulus.checkPassword" -}}
{{- $value := toString .value -}}
{{- if has $value (splitList "," (include "romulus.devPasswords" .)) -}}
{{- fail (printf "%s is one of romulus's development passwords: use a secret of your own (openssl rand -hex 32)" .what) -}}
{{- end -}}
{{- if lt (len $value) (int (.min | default 1)) -}}
{{- fail (printf "%s is shorter than %v characters: use a longer secret (openssl rand -hex 32)" .what .min) -}}
{{- end -}}
{{- end -}}

{{- define "romulus.validate" -}}
{{- $v := .Values -}}
{{- if and (not $v.host) (not $v.publicUrl) -}}
{{- fail "set host (the name people open romulus at, e.g. --set host=romulus.example.com), or publicUrl (e.g. http://localhost:8080 for a trial through kubectl port-forward)" -}}
{{- end -}}
{{- if and $v.ingress.enabled (not $v.host) -}}
{{- fail "the Ingress needs host (e.g. --set host=romulus.example.com), or turn it off: --set ingress.enabled=false" -}}
{{- end -}}
{{- if $v.publicUrl -}}
{{- if not (regexMatch "^https?://(\\[[0-9A-Fa-f:.]+\\]|[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?)(:[0-9]{1,5})?/?$" $v.publicUrl) -}}
{{- fail (printf "publicUrl %q: an origin without a path, e.g. https://romulus.example.com or http://localhost:8080" $v.publicUrl) -}}
{{- end -}}
{{- end -}}
{{- if $v.playgrounds.enabled -}}
{{- fail "playgrounds are not supported on Kubernetes yet: their runner drives Docker Engine through its socket, which cluster nodes do not offer, and every way around it weakens their isolation (docs/self-host/kubernetes.md, \"Playgrounds\"). Run romulus with deploy/self-host for playgrounds." -}}
{{- end -}}
{{- $methods := splitList "," ($v.auth.methods | nospace) -}}
{{- range $methods -}}
{{- if not (has . (list "password" "oidc")) -}}
{{- fail (printf "auth.methods: unknown sign-in method %q (password, oidc)" .) -}}
{{- end -}}
{{- end -}}
{{- if not (has "password" $methods) -}}
{{- fail "auth.methods must include password: the first admin is a password account (romulus-api setup). Use password,oidc for single sign-on beside it" -}}
{{- end -}}
{{- $oidc := $v.auth.oidc -}}
{{- if has "oidc" $methods -}}
{{- if not (and $oidc.issuer $oidc.clientId (or $oidc.clientSecret $oidc.existingSecret)) -}}
{{- fail "auth.methods has oidc: set auth.oidc.issuer, auth.oidc.clientId and auth.oidc.clientSecret (or auth.oidc.existingSecret)" -}}
{{- end -}}
{{- end -}}
{{- if and (eq $v.auth.signup "open") (not $v.smtp.host) -}}
{{- fail "auth.signup=open needs mail to prove addresses: set smtp.host, or keep invite" -}}
{{- end -}}
{{- if $v.smtp.host -}}
{{- if not $v.smtp.from -}}
{{- fail "smtp.host is set: set smtp.from too, e.g. \"romulus <romulus@example.com>\"" -}}
{{- end -}}
{{- if and (or $v.smtp.username $v.smtp.password $v.smtp.existingSecret) (not (and $v.smtp.username (or $v.smtp.password $v.smtp.existingSecret))) -}}
{{- fail "smtp: set both username and password (or existingSecret), or neither" -}}
{{- end -}}
{{- end -}}
{{- $ext := $v.postgresql.external -}}
{{- if $ext.enabled -}}
{{- if not (and $ext.existingSecret $ext.existingSecretUrlKey) -}}
{{- if not $ext.host -}}
{{- fail "postgresql.external.enabled: set postgresql.external.host (or existingSecret with existingSecretUrlKey)" -}}
{{- end -}}
{{- if not (or $ext.password $ext.existingSecret) -}}
{{- fail "postgresql.external.enabled: set postgresql.external.password or postgresql.external.existingSecret" -}}
{{- end -}}
{{- end -}}
{{- if and $ext.password (not $ext.existingSecret) -}}
{{- include "romulus.checkPassword" (dict "value" $ext.password "what" "postgresql.external.password") -}}
{{- end -}}
{{- end -}}
{{- range ($v.sandbox.extraMajors | default list) -}}
{{- if not (has (toString .) (list "15" "16" "18")) -}}
{{- fail (printf "sandbox.extraMajors: %v is not one (15, 16, 18; 17 always runs)" .) -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/* A pod running backup.sh with ARGS on the app database, into the
     backups volume. Beside the bundled Postgres (podAffinity): its pod
     mounts the same ReadWriteOnce volume. romulus.backupPod (dict "root" $
     "args" (list "nightly")) */}}
{{- define "romulus.backupPod" -}}
{{- $root := .root -}}
{{- $b := $root.Values.backup -}}
metadata:
  labels:
    {{- include "romulus.podLabels" (dict "root" $root "component" "backup") | nindent 4 }}
spec:
  restartPolicy: Never
  serviceAccountName: {{ include "romulus.serviceAccountName" $root }}
  automountServiceAccountToken: false
  enableServiceLinks: false
  {{- with $root.Values.imagePullSecrets }}
  imagePullSecrets:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  securityContext:
    {{- include "romulus.podSecurity" (dict "uid" 999) | nindent 4 }}
  {{- if not $root.Values.postgresql.external.enabled }}
  affinity:
    podAffinity:
      requiredDuringSchedulingIgnoredDuringExecution:
        - labelSelector:
            matchLabels:
              {{- include "romulus.selectorLabels" (dict "root" $root "component" "postgresql") | nindent 14 }}
          topologyKey: kubernetes.io/hostname
  {{- with $root.Values.postgresql.tolerations }}
  tolerations:
    {{- toYaml . | nindent 4 }}
  {{- end }}
  {{- end }}
  containers:
    - name: backup
      image: {{ include "romulus.backupImage" $root }}
      imagePullPolicy: {{ $root.Values.postgresql.image.pullPolicy }}
      # The database first: a new pod's connections can be refused for a
      # moment, until the network policy lets its address through.
      command:
        - bash
        - -c
        - |
          for _ in $(seq 1 60); do
            pg_isready -q -t 2 -d "$ROMULUS_DB" && break
            sleep 2
          done
          exec bash /scripts/backup.sh "$@"
        - backup.sh
      args:
        {{- toYaml .args | nindent 8 }}
      env:
        {{- include "romulus.databaseEnv" $root | trim | nindent 8 }}
        - name: ROMULUS_DB
          value: $(DATABASE_URL)
        - name: BACKUP_KEEP_DAYS
          value: {{ $b.keepDays | quote }}
        - name: HOME
          value: /tmp
      securityContext:
        {{- include "romulus.containerSecurity" $root | nindent 8 }}
      resources:
        {{- toYaml $b.resources | nindent 8 }}
      volumeMounts:
        - name: backups
          mountPath: /backups
        - name: scripts
          mountPath: /scripts
          readOnly: true
        - name: tmp
          mountPath: /tmp
  volumes:
    - name: backups
      persistentVolumeClaim:
        claimName: {{ include "romulus.backupClaim" $root }}
    - name: scripts
      configMap:
        name: {{ include "romulus.fullname" $root }}-backup
        defaultMode: 0555
    - name: tmp
      emptyDir: {}
{{- end -}}


{{/* The API's environment: the Deployment's, and the setup Job's (the
     admin commands read the same configuration). */}}
{{- define "romulus.apiEnv" -}}
{{- $api := .Values.api -}}
{{- include "romulus.databaseEnv" . | trim }}
- name: ROMULUS_PUBLIC_URL
  value: {{ include "romulus.publicUrl" . | quote }}
# Built-in email and password sign-in, one organization (created
# by `romulus-api setup`), accounts from invitations unless
# auth.signup=open.
- name: ROMULUS_AUTH
  value: {{ .Values.auth.methods | nospace | quote }}
- name: ROMULUS_SIGNUP
  value: {{ .Values.auth.signup | quote }}
- name: ROMULUS_SINGLE_ORG
  value: "true"
- name: ROMULUS_DEV_SIGNIN
  value: "false"
{{- with $api.trustedProxies }}
- name: ROMULUS_TRUSTED_PROXIES
  value: {{ . | quote }}
{{- end }}
# A database compiles on its Postgres major's sandbox: 17
# always, the others when sandbox.extraMajors names them.
- name: ROMULUS_SANDBOX_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ include "romulus.secretName" . }}
      key: sandbox-password
{{- range splitList "," (include "romulus.majors" .) }}
- name: {{ if eq . "17" }}ROMULUS_SANDBOX_URL{{ else }}ROMULUS_SANDBOX_URL_PG{{ . }}{{ end }}
  value: {{ printf "postgres://sandbox_admin:$(ROMULUS_SANDBOX_PASSWORD)@%s:5432/postgres" (include "romulus.sandboxName" (dict "root" $ "major" .)) | quote }}
{{- end }}
{{- with .Values.smtp }}
{{- if .host }}
# Mail; off without a host.
- name: ROMULUS_SMTP_HOST
  value: {{ .host | quote }}
{{- with .port }}
- name: ROMULUS_SMTP_PORT
  value: {{ . | quote }}
{{- end }}
{{- with .tls }}
- name: ROMULUS_SMTP_TLS
  value: {{ . | quote }}
{{- end }}
{{- with .username }}
- name: ROMULUS_SMTP_USERNAME
  value: {{ . | quote }}
{{- end }}
{{- if or .password .existingSecret }}
- name: ROMULUS_SMTP_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ .existingSecret | default (include "romulus.envSecretName" $) }}
      key: {{ if .existingSecret }}{{ .existingSecretKey }}{{ else }}smtp-password{{ end }}
{{- end }}
- name: ROMULUS_MAIL_FROM
  value: {{ .from | quote }}
{{- end }}
{{- end }}
{{- with .Values.auth.oidc }}
{{- if and .issuer (has "oidc" (splitList "," ($.Values.auth.methods | nospace))) }}
# Single sign-on with your own OpenID Connect provider.
- name: ROMULUS_OIDC_ISSUER
  value: {{ .issuer | quote }}
- name: ROMULUS_OIDC_CLIENT_ID
  value: {{ .clientId | quote }}
- name: ROMULUS_OIDC_CLIENT_SECRET
  valueFrom:
    secretKeyRef:
      name: {{ .existingSecret | default (include "romulus.envSecretName" $) }}
      key: {{ if .existingSecret }}{{ .existingSecretKey }}{{ else }}oidc-client-secret{{ end }}
{{- end }}
{{- end }}
- name: RUST_LOG
  value: {{ $api.logLevel | quote }}
{{- with $api.extraEnv }}
{{- toYaml . | nindent 0 }}
{{- end }}
{{- end -}}
