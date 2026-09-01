# Kubernetes example

Create the TLS Secret outside Git, inspect the rendered objects, then apply them:

```console
kubectl create secret tls relay-tls --cert=tls.crt --key=tls.key --dry-run=client -o yaml | kubectl apply -f -
kubectl kustomize deploy/kubernetes
kubectl apply -k deploy/kubernetes
```

The admin port is intentionally absent from the Service. Relay's non-loopback admin bind is an
explicit opt-in so kubelet probes can reach the pod; the NetworkPolicy permits port 9090 only from
the `monitoring` namespace. Adapt that policy to the cluster's probe networking before deployment.

Replace the example image reference with an immutable digest for production. The referenced
`relay-tls` Secret and application Service must exist before the pods become ready.
