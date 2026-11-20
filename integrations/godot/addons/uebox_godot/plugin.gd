@tool
extends EditorPlugin

# The bridge is inactive unless all three per-editor environment variables are set.
# Read-only queries plus one guarded, undo-able scene node rename. No generic
# command or code execution is exposed, and nothing auto-saves.
const MAX_REQUEST_BYTES := 32768
const MAX_RESPONSE_BYTES := 1048576
const MAX_CLIENTS := 16
const MAX_NODES := 2048
const MAX_SCENE_NODE_BYTES := 524288
const MAX_NODE_NAME_CHARS := 1024
const MAX_NODE_PATH_BYTES := 8192
const MAX_METADATA_CHARS := 4096
const CLIENT_LIFETIME_MS := 3000
const TICKET_LIFETIME_MS := 60000
const MAX_TICKETS := 64

var _server := TCPServer.new()
var _clients: Array[Dictionary] = []
var _token := ""
var _instance_id := ""
var _project_path := ""
var _tickets: Dictionary = {}


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
    _tickets.clear()


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
    _purge_expired_tickets()
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
        "scene.prepare_rename":
            result = _prepare_rename(request)
            if result.has("_error"):
                return _error(request_id, result._error.code, result._error.message)
        "scene.apply_rename":
            result = _apply_rename(request)
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


func _prepare_rename(request: Dictionary) -> Dictionary:
    var params: Variant = request.get("params")
    if not params is Dictionary or not params.get("node_path") is String or not params.get("new_name") is String:
        return {"_error": {"code": "invalid_request", "message": "Rename parameters are missing or invalid"}}
    var node_path: String = params.get("node_path")
    var new_name: String = params.get("new_name")
    if not _valid_node_name(new_name):
        return {"_error": {"code": "invalid_new_name", "message": "New node name is not allowed"}}
    var root := EditorInterface.get_edited_scene_root()
    if root == null:
        return {"_error": {"code": "no_scene", "message": "No scene is open in the editor"}}
    if node_path == ".":
        return {"_error": {"code": "root_rejected", "message": "The scene root cannot be renamed"}}
    if node_path.to_utf8_buffer().size() > MAX_NODE_PATH_BYTES:
        return {"_error": {"code": "node_not_found", "message": "Node path is too long to exist"}}
    var node := root.get_node_or_null(node_path)
    if node == null or not root.is_ancestor_of(node) or node.owner != root:
        return {"_error": {"code": "node_not_found", "message": "Node is not part of the edited scene"}}
    var parent := node.get_parent()
    if parent == null:
        return {"_error": {"code": "node_not_found", "message": "Node is not part of the edited scene"}}
    _purge_expired_tickets()
    if _tickets.size() >= MAX_TICKETS:
        return {"_error": {"code": "too_many_tickets", "message": "Too many outstanding rename tickets"}}
    var scene_path: Variant = root.scene_file_path
    if scene_path == "":
        scene_path = null
    var resolved_path := _relative_path(root, node)
    var ticket := _new_ticket()
    _tickets[ticket] = {
        "scene_id": str(root.get_instance_id()),
        "node_id": str(node.get_instance_id()),
        "parent_id": str(parent.get_instance_id()),
        "node_path": resolved_path,
        "old_name": str(node.name),
        "new_name": new_name,
        "expires_at": Time.get_ticks_msec() + TICKET_LIFETIME_MS,
    }
    return {
        "node_path": resolved_path,
        "old_name": str(node.name),
        "new_name": new_name,
        "scene_path": scene_path,
        "scene_id": str(root.get_instance_id()),
        "node_id": str(node.get_instance_id()),
        "ticket": ticket,
        "expires_in_ms": TICKET_LIFETIME_MS,
    }


func _apply_rename(request: Dictionary) -> Dictionary:
    var params: Variant = request.get("params")
    if not params is Dictionary or not params.get("ticket") is String:
        return _not_executed("invalid_request")
    var ticket: String = params.get("ticket")
    var prepared: Variant = _tickets.get(ticket)
    if prepared == null:
        return _not_executed("expired_or_consumed")
    _tickets.erase(ticket)
    if int(prepared.expires_at) <= Time.get_ticks_msec():
        return _not_executed("expired_or_consumed")
    var root := EditorInterface.get_edited_scene_root()
    if root == null or str(root.get_instance_id()) != str(prepared.scene_id):
        return _not_executed("context_changed")
    var target: Object = instance_from_id(int(prepared.node_id))
    if target == null or not target is Node:
        return _not_executed("node_changed")
    var node: Node = target
    if node == root or not root.is_ancestor_of(node) or node.owner != root:
        return _not_executed("node_changed")
    var parent := node.get_parent()
    if parent == null or str(parent.get_instance_id()) != str(prepared.parent_id):
        return _not_executed("node_changed")
    if _relative_path(root, node) != str(prepared.node_path):
        return _not_executed("node_changed")
    var old_name: String = str(prepared.old_name)
    if str(node.name) != old_name:
        return _not_executed("name_changed")
    var new_name: String = str(prepared.new_name)
    for sibling_index in range(parent.get_child_count()):
        var sibling := parent.get_child(sibling_index)
        if sibling != node and sibling.name == new_name:
            return _not_executed("name_collision")
    var undo := get_undo_redo()
    if undo == null:
        return _not_executed("undo_unavailable")
    undo.create_action("Rename Node")
    undo.add_do_method(node, "set_name", new_name)
    undo.add_undo_method(node, "set_name", old_name)
    undo.commit_action()
    if not is_instance_valid(root) or not is_instance_valid(node):
        return {"status": "unknown"}
    var actual_name := str(node.name)
    var node_id := str(node.get_instance_id())
    var scene_id := str(root.get_instance_id())
    if actual_name != new_name:
        return {"status": "mismatch", "actual_name": actual_name, "scene_id": scene_id, "node_id": node_id}
    return {"status": "applied", "actual_name": actual_name, "scene_id": scene_id, "node_id": node_id}


func _relative_path(root: Node, node: Node) -> String:
    var parts := PackedStringArray()
    var cursor: Node = node
    while cursor != root:
        parts.append(str(cursor.name))
        var next := cursor.get_parent()
        if next == null:
            return ""
        cursor = next
    if parts.is_empty():
        return "."
    parts.reverse()
    return "/".join(parts)


func _not_executed(reason: String) -> Dictionary:
    return {"status": "not_executed", "reason": reason}


func _new_ticket() -> String:
    var crypto := Crypto.new()
    return crypto.generate_random_bytes(16).hex_encode()


func _purge_expired_tickets() -> void:
    var now := Time.get_ticks_msec()
    for key in _tickets.keys():
        var entry: Dictionary = _tickets[key]
        if int(entry.expires_at) <= now:
            _tickets.erase(key)


func _valid_node_name(value: String) -> bool:
    if value.is_empty() or value.length() > MAX_NODE_NAME_CHARS:
        return false
    if value == "." or value == "..":
        return false
    for character in value:
        var code := character.unicode_at(0)
        if code < 32 or code == 127 or character == "/" or character == ":":
            return false
    return true
