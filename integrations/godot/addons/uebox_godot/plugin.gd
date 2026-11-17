@tool
extends EditorPlugin

# The bridge is inactive unless all three per-editor environment variables are set.
# No methods exposed by this server modify the project or editor scene.
const MAX_REQUEST_BYTES := 8192
const MAX_RESPONSE_BYTES := 1048576
const MAX_CLIENTS := 16
const MAX_NODES := 2048
const MAX_SCENE_NODE_BYTES := 524288
const MAX_NODE_NAME_CHARS := 1024
const MAX_NODE_PATH_BYTES := 8192
const MAX_METADATA_CHARS := 4096
const CLIENT_LIFETIME_MS := 3000

var _server := TCPServer.new()
var _clients: Array[Dictionary] = []
var _token := ""
var _instance_id := ""
var _project_path := ""


func _enter_tree() -> void:
    var port_text := OS.get_environment("UEBOX_GODOT_PORT")
    _token = OS.get_environment("UEBOX_GODOT_TOKEN")
    _instance_id = OS.get_environment("UEBOX_GODOT_INSTANCE_ID")
    if not port_text.is_valid_int() or not _is_hex(_token, 64) or not _is_hex(_instance_id, 32):
        return
    var port := port_text.to_int()
    if port < 1 or port > 65535:
        return
    _project_path = ProjectSettings.globalize_path("res://")
    # On POSIX a backslash can be a filename character, not a separator.
    if OS.get_name() != "Windows" and _project_path.contains("\\"):
        push_warning("UEBox Godot bridge cannot identify a project with a literal backslash")
        return
    _project_path = _project_path.simplify_path()
    if OS.get_name() == "Windows":
        _project_path = _project_path.replace("\\", "/")
    else:
        # Godot can normalize a literal backslash before exposing res://.
        # Compare cwd-relative filesystem objects with the reported project.
        var filesystem := DirAccess.open("/")
        if filesystem == null or not filesystem.is_equivalent("..", _project_path.get_base_dir()) or not filesystem.is_equivalent("project.godot", _project_path.path_join("project.godot")):
            push_warning("UEBox Godot bridge cannot verify the actual project directory")
            return
    while _project_path.length() > 1 and _project_path.ends_with("/"):
        _project_path = _project_path.left(_project_path.length() - 1)
    if _project_path.to_utf8_buffer().size() > 4096:
        push_warning("UEBox Godot bridge project path is too long")
        return
    var error := _server.listen(port, "127.0.0.1")
    if error != OK:
        push_warning("UEBox Godot bridge could not bind the requested loopback port: %s" % error)
        return
    set_process(true)


func _exit_tree() -> void:
    set_process(false)
    for client in _clients:
        var socket: StreamPeerTCP = client.socket
        socket.disconnect_from_host()
    _clients.clear()
    _server.stop()
    _token = ""


func _is_hex(value: String, length: int) -> bool:
    if value.length() != length:
        return false
    for character in value:
        if not "0123456789abcdef".contains(character):
            return false
    return true


func _token_matches(candidate: String) -> bool:
    var actual := _token.to_utf8_buffer()
    var supplied := candidate.to_utf8_buffer()
    if actual.size() != supplied.size():
        return false
    var difference := 0
    for index in range(actual.size()):
        difference |= actual[index] ^ supplied[index]
    return difference == 0


func _process(_delta: float) -> void:
    if not _server.is_listening():
        return
    # Bound work per frame and never wait on a client in the editor thread.
    for _index in range(8):
        if not _server.is_connection_available():
            break
        var socket := _server.take_connection()
        if socket == null:
            break
        if _clients.size() >= MAX_CLIENTS:
            socket.disconnect_from_host()
        else:
            socket.set_no_delay(true)
            _clients.append({"socket": socket, "incoming": PackedByteArray(),
                "outgoing": PackedByteArray(), "offset": 0, "started": Time.get_ticks_msec()})

    for index in range(_clients.size() - 1, -1, -1):
        var client: Dictionary = _clients[index]
        var socket: StreamPeerTCP = client.socket
        socket.poll()
        if socket.get_status() != StreamPeerTCP.STATUS_CONNECTED or Time.get_ticks_msec() - client.started > CLIENT_LIFETIME_MS:
            socket.disconnect_from_host()
            _clients.remove_at(index)
            continue
        var outgoing: PackedByteArray = client.outgoing
        if outgoing.is_empty():
            var available := socket.get_available_bytes()
            if available > 0:
                var incoming: PackedByteArray = client.incoming
                var received := socket.get_partial_data(mini(available, MAX_REQUEST_BYTES + 1 - incoming.size()))
                if received[0] != OK:
                    socket.disconnect_from_host()
                    _clients.remove_at(index)
                    continue
                incoming.append_array(received[1])
                client.incoming = incoming
                var newline := incoming.find(10)
                if newline >= 0:
                    if newline > MAX_REQUEST_BYTES:
                        socket.disconnect_from_host()
                        _clients.remove_at(index)
                        continue
                    var response := _handle(incoming.slice(0, newline).get_string_from_utf8())
                    outgoing = (JSON.stringify(response) + "\n").to_utf8_buffer()
                    if outgoing.size() > MAX_RESPONSE_BYTES:
                        outgoing = (JSON.stringify(_error(response.get("request_id", ""), "response_too_large", "Result exceeds the response limit")) + "\n").to_utf8_buffer()
                    client.outgoing = outgoing
                elif incoming.size() > MAX_REQUEST_BYTES:
                    socket.disconnect_from_host()
                    _clients.remove_at(index)
                    continue
        if not outgoing.is_empty():
            var offset: int = client.offset
            var end := mini(offset + 32768, outgoing.size())
            var sent := socket.put_partial_data(outgoing.slice(offset, end))
            if sent[0] != OK:
                socket.disconnect_from_host()
                _clients.remove_at(index)
                continue
            client.offset = offset + int(sent[1])
            if client.offset >= outgoing.size():
                socket.disconnect_from_host()
                _clients.remove_at(index)


func _error(request_id: String, code: String, message: String) -> Dictionary:
    return {"version": 1, "request_id": request_id, "error": {"code": code, "message": message}}


func _handle(line: String) -> Dictionary:
    var parsed: Variant = JSON.parse_string(line)
    if not parsed is Dictionary:
        return _error("", "invalid_request", "Expected a JSON object")
    var request: Dictionary = parsed
    var request_id: Variant = request.get("request_id")
    if not request_id is String or request_id.is_empty() or request_id.length() > 128:
        return _error("", "invalid_request", "Invalid request ID")
    if request.get("version") != 1:
        return _error(request_id, "invalid_request", "Unsupported protocol version")
    var token: Variant = request.get("token")
    if not token is String or not _token_matches(token):
        return _error(request_id, "unauthorized", "Authentication failed")
    var target: Variant = request.get("target")
    if not target is Dictionary or target.get("project_path") != _project_path or target.get("instance_id") != _instance_id:
        return _error(request_id, "wrong_target", "Project or editor instance does not match")
    var command: Variant = request.get("command")
    var result: Dictionary
    match command:
        "project.info":
            result = {"name": str(ProjectSettings.get_setting("application/config/name", "")),
                "godot_version": str(Engine.get_version_info().get("string", ""))}
        "scene.tree":
            result = _scene_tree()
            if result.has("_error"):
                return _error(request_id, "response_too_large", result._error)
        _:
            return _error(request_id, "unknown_command", "Command is not available")
    return {"version": 1, "request_id": request_id,
        "identity": {"project_path": _project_path, "instance_id": _instance_id}, "result": result}


func _scene_tree() -> Dictionary:
    var root := EditorInterface.get_edited_scene_root()
    if root == null:
        return {"scene_path": null, "nodes": [], "truncated": false}
    var scene_path: Variant = root.scene_file_path
    if scene_path == "":
        scene_path = null
    elif scene_path.length() > MAX_METADATA_CHARS:
        return {"_error": "Scene path exceeds the response limit"}

    # Queue only node references and parent indices, never full descendant paths.
    # Construct one bounded node at a time and account for its exact JSON bytes.
    var queue: Array[Dictionary] = [{"node": root, "parent": -1}]
    var nodes: Array[Dictionary] = []
    var truncated := false
    var cursor := 0
    var used_bytes := 0
    while cursor < queue.size():
        var queued: Dictionary = queue[cursor]
        var node: Node = queued.node
        cursor += 1
        if len(node.name) > MAX_NODE_NAME_CHARS:
            truncated = true
            continue
        var name := str(node.name)
        var parent_index: int = queued.parent
        var path := "."
        if parent_index >= 0:
            var parent_path: String = nodes[parent_index].path
            var prefix := "" if parent_path == "." else parent_path + "/"
            if prefix.to_utf8_buffer().size() + name.to_utf8_buffer().size() > MAX_NODE_PATH_BYTES:
                truncated = true
                continue
            path = prefix + name
        var node_class := node.get_class()
        if node_class.length() > MAX_NODE_NAME_CHARS:
            truncated = true
            continue
        var entry := {"path": path, "name": name, "class": node_class}
        var entry_size := JSON.stringify(entry).to_utf8_buffer().size() + 1
        if used_bytes + entry_size > MAX_SCENE_NODE_BYTES:
            truncated = true
            break
        used_bytes += entry_size
        nodes.append(entry)
        var current_index := nodes.size() - 1
        for index in range(node.get_child_count()):
            if queue.size() >= MAX_NODES:
                truncated = true
                break
            queue.append({"node": node.get_child(index), "parent": current_index})
    return {"scene_path": scene_path, "nodes": nodes, "truncated": truncated}
