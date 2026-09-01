// @generated automatically by Diesel CLI.

diesel::table! {
    account_session (id) {
        id -> Text,
        account_id -> Text,
        device_id -> Text,
        encrypted_device_info -> Nullable<Text>,
        created_at -> BigInt,
        last_active_at -> BigInt,
        expires_at -> BigInt,
        revoked_at -> Nullable<BigInt>,
        legacy -> Bool,
        generation -> BigInt,
        pending_pairing -> Bool,
    }
}

diesel::table! {
    encrypted_sync (account_id, collection) {
        account_id -> Text,
        collection -> Text,
        revision -> BigInt,
        payload -> Text,
    }
}

diesel::table! {
    pairing_session (id) {
        id -> Text,
        version -> SmallInt,
        account_id -> Nullable<Text>,
        recipient_public_key -> Text,
        recipient_device_id -> Text,
        recipient_device_name -> Text,
        recipient_token_hash -> Text,
        approving_device_id -> Nullable<Text>,
        encrypted_payload -> Nullable<Text>,
        expires_at -> BigInt,
    }
}

diesel::table! {
    account (id) {
        id -> Text,
        name_hash -> Text,
        password_hash -> Nullable<Text>,
        oidc_sub -> Nullable<Text>,
        legacy_tokens_enabled -> Bool,
        session_generation -> BigInt,
    }
}

diesel::table! {
    channel (id) {
        id -> Text,
        name -> Text,
        avatar -> Nullable<Text>,
        verified -> Bool,
    }
}

diesel::table! {
    channel_playback_speed (account_id, channel_id) {
        account_id -> Text,
        channel_id -> Text,
        playback_speed -> Double,
    }
}

diesel::table! {
    playlist (id, account_id) {
        id -> Text,
        account_id -> Text,
        title -> Text,
        description -> Text,
        thumbnail_url -> Nullable<Text>,
    }
}

diesel::table! {
    playlist_bookmark (account_id, public_playlist_id) {
        account_id -> Text,
        public_playlist_id -> Text,
    }
}

diesel::table! {
    playlist_video_member (account_id, playlist_id, video_id) {
        account_id -> Text,
        playlist_id -> Text,
        video_id -> Text,
    }
}

diesel::table! {
    public_playlist (id) {
        id -> Text,
        title -> Text,
        description -> Text,
        thumbnail_url -> Nullable<Text>,
        uploader_id -> Text,
        video_count -> Nullable<Integer>,
    }
}

diesel::table! {
    subscription (account_id, channel_id) {
        account_id -> Text,
        channel_id -> Text,
    }
}

diesel::table! {
    subscription_group (id) {
        id -> Text,
        account_id -> Text,
        title -> Text,
    }
}

diesel::table! {
    subscription_group_member (subscription_group_id, channel_id) {
        subscription_group_id -> Text,
        channel_id -> Text,
    }
}

diesel::table! {
    video (id) {
        id -> Text,
        title -> Text,
        upload_date -> BigInt,
        uploader_id -> Text,
        thumbnail_url -> Text,
        duration -> Integer,
    }
}

diesel::table! {
    watch_history (video_id, account_id) {
        video_id -> Text,
        account_id -> Text,
        added_date -> BigInt,
        watched_state -> Text,
        position_millis -> Nullable<Integer>,
    }
}

diesel::joinable!(playlist -> account (account_id));
diesel::joinable!(account_session -> account (account_id));
diesel::joinable!(channel_playback_speed -> account (account_id));
diesel::joinable!(encrypted_sync -> account (account_id));
diesel::joinable!(pairing_session -> account (account_id));
diesel::joinable!(playlist_bookmark -> account (account_id));
diesel::joinable!(playlist_bookmark -> public_playlist (public_playlist_id));
diesel::joinable!(playlist_video_member -> account (account_id));
diesel::joinable!(playlist_video_member -> video (video_id));
diesel::joinable!(public_playlist -> channel (uploader_id));
diesel::joinable!(subscription -> account (account_id));
diesel::joinable!(subscription -> channel (channel_id));
diesel::joinable!(subscription_group -> account (account_id));
diesel::joinable!(subscription_group_member -> channel (channel_id));
diesel::joinable!(subscription_group_member -> subscription_group (subscription_group_id));
diesel::joinable!(video -> channel (uploader_id));
diesel::joinable!(watch_history -> account (account_id));
diesel::joinable!(watch_history -> video (video_id));

diesel::allow_tables_to_appear_in_same_query!(
    account,
    account_session,
    channel,
    channel_playback_speed,
    encrypted_sync,
    pairing_session,
    playlist,
    playlist_bookmark,
    playlist_video_member,
    public_playlist,
    subscription,
    subscription_group,
    subscription_group_member,
    video,
    watch_history,
);
