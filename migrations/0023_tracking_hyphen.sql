-- A fourth tracker type: a hyphen image added at the end of the email.
ALTER TABLE tracking_settings DROP CONSTRAINT IF EXISTS tracking_settings_tracker_type_check;
ALTER TABLE tracking_settings ADD CONSTRAINT tracking_settings_tracker_type_check
    CHECK (tracker_type IN ('text_image', 'signature_image', 'end_mark_image', 'hyphen_image'));
