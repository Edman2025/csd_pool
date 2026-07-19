update pool_settings
set value = 'false',
    updated_at = now()
where key = 'payouts_enabled'
  and value in ('true', '1', 'yes', 'on');
