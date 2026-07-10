for p in $(busctl --user --no-pager --list tree org.freedesktop.secrets | grep "wallet/"); do
  echo
  echo "== $p =="
  busctl --user get-property org.freedesktop.secrets "$p" org.freedesktop.Secret.Item Label
  busctl --user get-property org.freedesktop.secrets "$p" org.freedesktop.Secret.Item Attributes
done