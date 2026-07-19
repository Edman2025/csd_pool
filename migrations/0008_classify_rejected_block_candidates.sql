update blocks
set status = 'orphaned',
    orphaned_at = coalesce(orphaned_at, now())
where status = 'submitted'
  and submit_response_json->>'ok' = 'false'
  and (
    submit_response_json ? 'http_status'
    or submit_response_json->>'transport_error' ~
      'HTTP status client error \(4[0-9]{2} [A-Za-z ]+\)'
  );
